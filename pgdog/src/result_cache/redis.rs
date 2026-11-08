use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

// Additions for encryption
use aes_gcm::aead::OsRng;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use redis::AsyncCommands;
use regex::Regex;
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::{config::config, frontend::router::parser::OwnedTable, net::Parameters};

use super::{
    connection::{Connection, Target},
    entry::Entry,
    singleflight::Singleflight,
    ttl, xfetch, CacheableRequest, ResultCacheKey,
};

#[derive(Debug, Clone)]
pub struct ResultCacheConfig {
    pub enabled: bool,
    pub target: Target,
    pub expire_seconds: Option<u64>,
    pub max_entry_bytes: usize,
    pub encryption_key: Option<String>,
    pub xfetch_beta: f64,
    pub adaptive_ttl_max: Option<Duration>,
    pub key_prefix: String,
    pub safe_schema_list: Vec<Regex>,
    pub unsafe_schema_list: Vec<Regex>,
    pub safe_table_list: Vec<Regex>,
    pub unsafe_table_list: Vec<Regex>,
}

impl ResultCacheConfig {
    pub fn from_global_config() -> Option<Self> {
        let cfg = config();
        let rc = cfg.config.result_cache.clone()?;
        if !rc.enabled {
            return None;
        }

        Some(Self {
            enabled: rc.enabled,
            target: Target::new(
                rc.redis_mode,
                &rc.redis_url,
                &rc.redis_urls,
                rc.redis_sentinel_master.as_deref(),
            ),
            // Apply a default TTL when not provided.
            expire_seconds: rc.expire_seconds.or(Some(30)),
            max_entry_bytes: rc.max_entry_bytes.unwrap_or(512 * 1024),
            encryption_key: rc.encryption_key,
            xfetch_beta: rc.xfetch_beta.unwrap_or(1.0),
            adaptive_ttl_max: rc.adaptive_ttl_max_seconds.map(Duration::from_secs),
            key_prefix: rc
                .key_prefix
                .unwrap_or_else(|| "pgdog:result_cache".to_string()),
            safe_schema_list: compile_list(&rc.cache_safe_schema_list),
            unsafe_schema_list: compile_list(&rc.cache_unsafe_schema_list),
            safe_table_list: compile_list(&rc.cache_safe_table_list),
            unsafe_table_list: compile_list(&rc.cache_unsafe_table_list),
        })
    }

    pub fn ttl(&self) -> Option<Duration> {
        self.expire_seconds.map(Duration::from_secs)
    }

    pub fn table_allowed(&self, table: &OwnedTable) -> bool {
        let schema = table.schema.as_deref().unwrap_or_default();
        let table_only = table.name.as_str();
        let schema_table = if schema.is_empty() {
            table_only.to_string()
        } else {
            format!("{}.{}", schema, table_only)
        };

        // Schema deny takes precedence.
        if !schema.is_empty() && matches_any(&self.unsafe_schema_list, schema) {
            return false;
        }

        // Table deny takes precedence.
        if matches_any(&self.unsafe_table_list, &schema_table)
            || matches_any(&self.unsafe_table_list, table_only)
        {
            return false;
        }

        // If allow lists are empty, allow by default.
        let schema_ok = self.safe_schema_list.is_empty()
            || (!schema.is_empty() && matches_any(&self.safe_schema_list, schema));
        let table_ok = self.safe_table_list.is_empty()
            || matches_any(&self.safe_table_list, &schema_table)
            || matches_any(&self.safe_table_list, table_only);

        schema_ok && table_ok
    }
}

/// After failing to connect, how long to leave Redis alone. Queries skip the
/// cache meanwhile instead of each one waiting on a connection attempt.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Shared {
    target: Option<Target>,
    connection: Option<Connection>,
    prefix: String,
    /// Last failed connection attempt, and what it was for.
    failed: Option<(Instant, Target)>,
}

/// A cache hit.
#[derive(Debug)]
pub struct Cached {
    pub entry: Entry,
    /// XFetch picked this hit to recompute the entry ahead of expiry.
    pub refresh_early: bool,
}

/// Result cache shared by all clients of this process.
static CACHE: Lazy<RedisResultCache> = Lazy::new(RedisResultCache::default);

#[derive(Debug, Clone)]
pub struct RedisResultCache {
    shared: Arc<RwLock<Shared>>,
    singleflight: Singleflight,
}

impl Default for RedisResultCache {
    fn default() -> Self {
        Self {
            shared: Arc::new(RwLock::new(Shared {
                target: None,
                connection: None,
                prefix: "pgdog:result_cache".into(),
                failed: None,
            })),
            singleflight: Singleflight::default(),
        }
    }
}

impl RedisResultCache {
    /// The process-wide cache, if enabled and Redis is reachable.
    ///
    /// All clients share it, along with its Redis connection and in-flight queries.
    pub async fn global() -> Option<Self> {
        let cfg = ResultCacheConfig::from_global_config()?;
        let cache = CACHE.clone();
        cache.ensure_connected(&cfg).await?;
        Some(cache)
    }

    /// Queries in flight, to coalesce concurrent misses on the same key.
    pub fn singleflight(&self) -> &Singleflight {
        &self.singleflight
    }

    async fn ensure_connected(&self, cfg: &ResultCacheConfig) -> Option<()> {
        {
            let shared = self.shared.read();
            if shared.target.as_ref() == Some(&cfg.target)
                && shared.connection.is_some()
                && shared.prefix == cfg.key_prefix
            {
                return Some(());
            }
            if let Some((at, target)) = &shared.failed {
                if target == &cfg.target && at.elapsed() < RECONNECT_BACKOFF {
                    return None;
                }
            }
        }

        let connection = match Connection::open(&cfg.target).await {
            Ok(connection) => connection,
            Err(err) => {
                crate::stats::ResultCache::redis_error();
                warn!("result_cache: can't connect to {:?}: {}", cfg.target, err);
                self.shared.write().failed = Some((Instant::now(), cfg.target.clone()));
                return None;
            }
        };
        let mut shared = self.shared.write();
        shared.target = Some(cfg.target.clone());
        shared.connection = Some(connection);
        shared.prefix = cfg.key_prefix.clone();
        shared.failed = None;
        Some(())
    }

    /// After a timeout, a connection error, or a write refused by a former
    /// primary, stop using Redis for
    /// `RECONNECT_BACKOFF`, so queries don't each wait on it.
    fn unavailable(&self, err: &redis::RedisError) {
        if !makes_unavailable(err) {
            return;
        }

        let mut shared = self.shared.write();
        if let Some(target) = shared.target.take() {
            warn!("result_cache: Redis unavailable, skipping the cache for {RECONNECT_BACKOFF:?}");
            shared.failed = Some((Instant::now(), target));
        }
        shared.connection = None;
    }

    /// Handle to the shared Redis connection. Cheap to clone.
    fn connection(&self) -> Option<Connection> {
        self.shared.read().connection.clone()
    }

    /// Set the key's TTL in the background, so the client doesn't wait on it.
    fn extend(&self, redis_key: &str, ttl: Duration, mut conn: Connection) {
        let redis_key = redis_key.to_owned();
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
        tokio::spawn(async move {
            let extended: redis::RedisResult<bool> = conn.pexpire(&redis_key, ttl_ms).await;
            match extended {
                Ok(_) => crate::stats::ResultCache::ttl_extension(),
                Err(err) => {
                    crate::stats::ResultCache::redis_error();
                    warn!("result_cache ttl extension failed: {}", err);
                }
            }
        });
    }

    fn tag_set_key(prefix: &str, db: &str, table: &OwnedTable) -> String {
        let schema = table.schema.as_deref().unwrap_or("_");
        format!("{prefix}:tbl:{db}:{schema}.{}", table.name)
    }

    /// Counter bumped on every invalidation of the table. Never expires: if it
    /// did, the counter would restart and old entries would match again.
    fn generation_key(prefix: &str, db: &str, table: &OwnedTable) -> String {
        let schema = table.schema.as_deref().unwrap_or("_");
        format!("{prefix}:gen:{db}:{schema}.{}", table.name)
    }

    /// Current generation of each table, joined into a single string.
    ///
    /// It's part of the cache key, so a result computed before a table was
    /// invalidated is stored under a key no reader will look up again.
    async fn generations(&self, prefix: &str, db: &str, tables: &[OwnedTable]) -> Option<String> {
        if tables.is_empty() {
            return Some(String::new());
        }

        let keys: Vec<_> = tables
            .iter()
            .map(|table| Self::generation_key(prefix, db, table))
            .collect();
        let mut conn = self.connection()?;
        let generations: Vec<Option<u64>> =
            match redis::cmd("MGET").arg(&keys).query_async(&mut conn).await {
                Ok(generations) => generations,
                Err(err) => {
                    crate::stats::ResultCache::redis_error();
                    self.unavailable(&err);
                    warn!("result_cache generation lookup failed: {}", err);
                    return None;
                }
            };

        Some(
            generations
                .iter()
                .map(|generation| generation.unwrap_or(0).to_string())
                .collect::<Vec<_>>()
                .join(","),
        )
    }

    pub async fn build_key(
        &self,
        db: &str,
        user: &str,
        params: &Parameters,
        req: &CacheableRequest,
    ) -> Option<ResultCacheKey> {
        let cfg = ResultCacheConfig::from_global_config()?;
        self.ensure_connected(&cfg).await?;

        // Filter by table/schema patterns.
        if !req.tables.is_empty() && req.tables.iter().any(|t| !cfg.table_allowed(t)) {
            return None;
        }

        // Session signature (conservative MVP):
        // include search_path and role-ish identity (user already included).
        let search_path = params
            .get("search_path")
            .map(|v| v.to_string())
            .unwrap_or_default();
        let session_sig = format!("{:x}", md5::compute(format!("search_path={}", search_path)));

        // Query fingerprint, tied to the current generation of every table it reads.
        let generations = self.generations(&cfg.key_prefix, db, &req.tables).await?;
        let fingerprint = format!(
            "{:x}",
            md5::compute(format!("{}\0{}", req.query, generations))
        );

        let key = format!(
            "{}:v2:{}:{}:{}:{}:{}",
            cfg.key_prefix, db, user, session_sig, req.route_sig, fingerprint
        );

        Some(ResultCacheKey {
            redis_key: key,
            ttl: cfg.ttl(),
            max_entry_bytes: cfg.max_entry_bytes,
        })
    }

    /// Cached entry for `key`, if any, and whether this hit should refresh it early.
    pub async fn get(&self, key: &ResultCacheKey) -> Option<Cached> {
        let cfg = ResultCacheConfig::from_global_config()?;
        self.ensure_connected(&cfg).await?;
        let mut conn = self.connection()?;

        // Value and remaining TTL in one round trip.
        let (bytes, pttl): (Vec<u8>, i64) = match redis::pipe()
            .get(&key.redis_key)
            .pttl(&key.redis_key)
            .query_async(&mut conn)
            .await
        {
            Ok(reply) => reply,
            Err(err) => {
                crate::stats::ResultCache::redis_error();
                self.unavailable(&err);
                warn!("result_cache get failed: {}", err);
                return None;
            }
        };
        if bytes.is_empty() {
            return None;
        }

        let bytes = match &cfg.encryption_key {
            Some(enc_key) => {
                let Some(plaintext) = decrypt(enc_key, &bytes) else {
                    warn!(
                        "result_cache: failed to decrypt value for key: {}",
                        key.redis_key
                    );
                    return None;
                };
                plaintext
            }
            None => bytes,
        };
        let entry = Entry::decode(&bytes)?;

        // PTTL is negative when the key has no expiry (or just expired).
        let remaining = u64::try_from(pttl).ok().map(Duration::from_millis);
        let refresh_early = remaining.is_some_and(|remaining| {
            xfetch::should_refresh(remaining, entry.delta, cfg.xfetch_beta, xfetch::draw())
        });

        // Adaptive TTL. Skipped when this hit refreshes the entry anyway.
        if let (false, Some(remaining), Some(ttl), Some(max_age)) =
            (refresh_early, remaining, key.ttl, cfg.adaptive_ttl_max)
        {
            let age = entry.age(SystemTime::now());
            if let Some(extended) = ttl::extended_ttl(age, remaining, ttl, max_age) {
                self.extend(&key.redis_key, extended, conn);
            }
        }

        Some(Cached {
            entry,
            refresh_early,
        })
    }

    pub async fn set(&self, key: &ResultCacheKey, entry: &Entry) {
        let Some(cfg) = ResultCacheConfig::from_global_config() else {
            return;
        };

        if entry.payload.is_empty() {
            return;
        }

        let encoded = entry.encode();
        let final_payload = if let Some(enc_key) = &cfg.encryption_key {
            match encrypt(enc_key, &encoded) {
                Some(encrypted) => encrypted,
                None => {
                    warn!(
                        "result_cache: failed to encrypt value for key: {}",
                        key.redis_key
                    );
                    return; // Don't cache if encryption fails
                }
            }
        } else {
            encoded
        };

        if final_payload.len() > key.max_entry_bytes {
            return;
        }

        if self.ensure_connected(&cfg).await.is_none() {
            return;
        }

        let Some(mut conn) = self.connection() else {
            return;
        };

        let res: redis::RedisResult<()> = match key.ttl {
            Some(ttl) if ttl > Duration::ZERO => {
                conn.set_ex(
                    &key.redis_key,
                    final_payload.as_slice(),
                    ttl.as_secs() as u64,
                )
                .await
            }
            _ => conn.set(&key.redis_key, final_payload.as_slice()).await,
        };

        if let Err(err) = res {
            crate::stats::ResultCache::redis_error();
            self.unavailable(&err);
            warn!("result_cache set failed: {}", err);
        }
    }

    pub async fn set_with_table_tags(
        &self,
        db: &str,
        key: &ResultCacheKey,
        entry: &Entry,
        tables: &[OwnedTable],
    ) {
        let Some(cfg) = ResultCacheConfig::from_global_config() else {
            return;
        };

        if tables.iter().any(|t| !cfg.table_allowed(t)) {
            return;
        }

        self.set(key, entry).await;

        if entry.payload.is_empty() {
            return;
        }

        if self.ensure_connected(&cfg).await.is_none() {
            return;
        }

        // Outlive the entries it points to, including adaptive TTL extensions.
        let ttl = key.ttl.unwrap_or_else(|| Duration::from_secs(30));
        let lifetime = cfg.adaptive_ttl_max.map_or(ttl, |max_age| max_age.max(ttl));
        let set_ttl_secs = (lifetime.as_secs().saturating_mul(2)).max(60);

        let Some(mut conn) = self.connection() else {
            return;
        };

        for table in tables {
            let set_key = Self::tag_set_key(&cfg.key_prefix, db, table);
            let _: redis::RedisResult<()> = conn.sadd(&set_key, &key.redis_key).await;
            let _: redis::RedisResult<()> = conn.expire(&set_key, set_ttl_secs as i64).await;
        }
    }

    pub async fn invalidate_tables(&self, db: &str, tables: &[OwnedTable]) {
        let Some(cfg) = ResultCacheConfig::from_global_config() else {
            return;
        };
        if self.ensure_connected(&cfg).await.is_none() {
            return;
        }

        let Some(mut conn) = self.connection() else {
            return;
        };

        for table in tables {
            // Bump the generation first: it's what keeps results computed
            // before this write from being served. Deleting tagged keys
            // below only frees memory sooner.
            let generation_key = Self::generation_key(&cfg.key_prefix, db, table);
            let bumped: redis::RedisResult<u64> = conn.incr(&generation_key, 1).await;
            if let Err(err) = bumped {
                crate::stats::ResultCache::redis_error();
                self.unavailable(&err);
                warn!("result_cache generation bump failed: {}", err);
            }

            let set_key = Self::tag_set_key(&cfg.key_prefix, db, table);
            let keys: redis::RedisResult<Vec<String>> = conn.smembers(&set_key).await;
            match keys {
                Ok(keys) => {
                    if !keys.is_empty() {
                        let _: redis::RedisResult<()> = conn.del(keys).await;
                    }
                    let _: redis::RedisResult<()> = conn.del(&set_key).await;
                }
                Err(err) => {
                    crate::stats::ResultCache::redis_error();
                    self.unavailable(&err);
                    warn!("result_cache invalidate failed: {}", err);
                }
            }
        }
    }
}

/// Errors meaning Redis can't be used right now: it's unreachable, too slow,
/// or the connection points at a former primary (READONLY, e.g. after a
/// Sentinel failover). Errors about a single command don't count.
fn makes_unavailable(err: &redis::RedisError) -> bool {
    err.is_timeout()
        || err.is_io_error()
        || err.is_connection_dropped()
        || err.is_connection_refusal()
        || err.kind() == redis::ErrorKind::Server(redis::ServerErrorKind::ReadOnly)
}

fn get_cipher(key_str: &str) -> Aes256Gcm {
    // Use SHA-256 to derive a 32-byte key from the user-provided string.
    let mut hasher = Sha256::new();
    hasher.update(key_str.as_bytes());
    let binding = hasher.finalize();
    let key = Key::<Aes256Gcm>::from_slice(&binding);
    Aes256Gcm::new(key)
}

fn encrypt(key_str: &str, plaintext: &[u8]) -> Option<Vec<u8>> {
    let cipher = get_cipher(key_str);
    // 96-bits (12 bytes) is the standard nonce size for AES-GCM.
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    match cipher.encrypt(&nonce, plaintext) {
        Ok(ciphertext) => {
            let mut result = nonce.to_vec();
            result.extend_from_slice(&ciphertext);
            Some(result)
        }
        Err(_) => None,
    }
}

fn decrypt(key_str: &str, ciphertext_with_nonce: &[u8]) -> Option<Vec<u8>> {
    const NONCE_SIZE: usize = 12;
    if ciphertext_with_nonce.len() < NONCE_SIZE {
        return None; // Not long enough to contain a nonce
    }
    let cipher = get_cipher(key_str);
    let (nonce_bytes, ciphertext) = ciphertext_with_nonce.split_at(NONCE_SIZE);
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ciphertext).ok()
}

fn compile_list(patterns: &[String]) -> Vec<Regex> {
    patterns.iter().filter_map(|p| Regex::new(p).ok()).collect()
}

fn matches_any(patterns: &[Regex], value: &str) -> bool {
    patterns.iter().any(|re| re.is_match(value))
}

#[cfg(test)]
mod test {
    use std::io;

    use redis::{ErrorKind, RedisError, ServerErrorKind};

    use super::makes_unavailable;

    #[test]
    fn test_read_only_makes_redis_unavailable() {
        let err = RedisError::from((ErrorKind::Server(ServerErrorKind::ReadOnly), "READONLY"));
        assert!(makes_unavailable(&err));
    }

    #[test]
    fn test_network_errors_make_redis_unavailable() {
        for kind in [
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::TimedOut,
        ] {
            assert!(
                makes_unavailable(&RedisError::from(io::Error::from(kind))),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn test_command_errors_dont_make_redis_unavailable() {
        let err = RedisError::from((
            ErrorKind::Server(ServerErrorKind::ResponseError),
            "WRONGTYPE",
        ));
        assert!(!makes_unavailable(&err));
    }
}
