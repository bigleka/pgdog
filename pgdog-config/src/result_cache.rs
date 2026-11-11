use std::{fs::read_to_string, path::PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResultCache {
    /// Enable Redis/RESP result cache.
    #[serde(default)]
    pub enabled: bool,

    /// Redis connection URL, e.g. redis://127.0.0.1:6379
    #[serde(default)]
    pub redis_url: String,

    /// How PgDog connects to Redis.
    ///
    /// _Default:_ `standalone`
    #[serde(default)]
    pub redis_mode: RedisMode,

    /// Redis nodes for `redis_mode = "cluster"` (one or more cluster nodes to
    /// discover the cluster from) or `"sentinel"` (the Sentinels). Falls back
    /// to `redis_url` when empty.
    #[serde(default)]
    pub redis_urls: Vec<String>,

    /// Name of the primary Sentinel monitors, for `redis_mode = "sentinel"`.
    pub redis_sentinel_master: Option<String>,

    /// How long connecting to Redis may take before it counts as down
    /// (milliseconds). See "Redis timeouts" in docs/RESULT_CACHE.md before
    /// changing it.
    ///
    /// _Default:_ `500`
    pub redis_connect_timeout_ms: Option<u64>,

    /// How long a Redis command may take before Redis counts as down
    /// (milliseconds). Every cacheable query waits on Redis at least once, so
    /// this is also the most a slow Redis adds to a query.
    ///
    /// _Default:_ `500`
    pub redis_response_timeout_ms: Option<u64>,

    /// How many times connecting to Redis is retried before it counts as down.
    ///
    /// _Default:_ `1`
    pub redis_connect_retries: Option<u32>,

    /// Once Redis counts as down, how long the cache is skipped, queries
    /// going straight to Postgres, before connecting again (milliseconds).
    /// `0` tries again on the next query.
    ///
    /// _Default:_ `5000`
    pub redis_unavailable_backoff_ms: Option<u64>,

    /// Optional expiration time for cached entries (seconds).
    pub expire_seconds: Option<u64>,

    /// Maximum entry size to cache (bytes). If not set, PgDog uses a safe default.
    pub max_entry_bytes: Option<usize>,

    /// Key prefix in Redis.
    pub key_prefix: Option<String>,

    /// Encrypts cached results in Redis (AES-256-GCM), so whoever can read
    /// Redis can't read the data. Use a long random string, e.g.
    /// `openssl rand -base64 32`.
    ///
    /// Prefer `encryption_key_file` or the `PGDOG_RESULT_CACHE_ENCRYPTION_KEY`
    /// environment variable, which overrides both, to keep the key out of
    /// this file.
    ///
    /// _Default:_ none, results are stored unencrypted
    pub encryption_key: Option<String>,

    /// File holding `encryption_key`, e.g. a mounted Kubernetes Secret.
    /// Trailing whitespace and newlines are ignored. Can't be set together with
    /// `encryption_key`. Read when the configuration is loaded or reloaded.
    pub encryption_key_file: Option<PathBuf>,

    /// How eagerly hits refresh entries before they expire (XFetch).
    ///
    /// A hit refreshes its entry when `-compute_time * xfetch_beta * ln(rand)`
    /// reaches the entry's remaining TTL, so hot and slow queries are
    /// recomputed by one client ahead of expiry instead of by every client
    /// after it. Values above `1.0` refresh earlier, below later; `0` disables
    /// early refresh.
    ///
    /// _Default:_ `1.0`
    pub xfetch_beta: Option<f64>,

    /// Enable adaptive TTL: each hit pushes the entry's expiry to
    /// `expire_seconds` from now, so frequently read entries stay cached,
    /// but never longer than this many seconds after the query ran.
    ///
    /// **Note:** changes made outside PgDog can stay invisible for up to
    /// this long, instead of `expire_seconds`. Writes through PgDog still
    /// invalidate entries immediately.
    ///
    /// _Default:_ disabled
    pub adaptive_ttl_max_seconds: Option<u64>,

    /// Coalesce misses between PgDogs sharing Redis: the first one to miss
    /// on a query takes a lock in Redis and runs it, the others wait for its
    /// result instead of running the same query.
    ///
    /// The lock is renewed while the query runs and expires this many
    /// milliseconds after the PgDog holding it stops renewing it, e.g. when
    /// it dies. That's how long the others wait before running the query
    /// themselves.
    ///
    /// `0` disables the lock: each PgDog only coalesces its own clients.
    ///
    /// _Default:_ `1000`
    pub singleflight_lock_ms: Option<u64>,

    /// With replicas: for this many milliseconds after a write to a table,
    /// reads of that table go to the primary instead of a replica that may
    /// not have the write yet. Covers the client that wrote, other clients,
    /// and other PgDogs sharing Redis, and keeps stale replica results out of
    /// the cache. Set it above your replicas' usual lag.
    ///
    /// `0` disables it. No effect without replicas.
    ///
    /// _Default:_ `1000`
    pub read_after_write_window_ms: Option<u64>,

    /// Use the cache in read-only transactions that don't name an isolation
    /// level (`BEGIN READ ONLY`), as if they were `READ COMMITTED`.
    ///
    /// Read-only transactions that say `ISOLATION LEVEL READ COMMITTED` use
    /// the cache regardless, and `REPEATABLE READ` or `SERIALIZABLE` ones
    /// never do: each of their statements must see the same snapshot.
    ///
    /// **Note:** PgDog can't see `default_transaction_isolation` set with
    /// `ALTER ROLE` or `ALTER DATABASE`. Don't enable this if a role or
    /// database defaults to `repeatable read` or `serializable`.
    ///
    /// _Default:_ `false`
    #[serde(default)]
    pub cache_read_only_transactions: bool,

    /// Optional allow-list of schemas whose tables can be cached.
    ///
    /// Each entry is treated as a regular expression.
    #[serde(default)]
    pub cache_safe_schema_list: Vec<String>,

    /// Optional deny-list of schemas whose tables should never be cached.
    ///
    /// Takes precedence over `cache_safe_schema_list`.
    /// Each entry is treated as a regular expression.
    #[serde(default)]
    pub cache_unsafe_schema_list: Vec<String>,

    /// Optional allow-list of tables whose results can be cached.
    ///
    /// Each entry is treated as a regular expression and matched against
    /// either `table` or `schema.table` when schema is present.
    #[serde(default)]
    pub cache_safe_table_list: Vec<String>,

    /// Optional deny-list of tables whose results should never be cached.
    ///
    /// Takes precedence over `cache_safe_table_list`.
    /// Each entry is treated as a regular expression and matched against
    /// either `table` or `schema.table` when schema is present.
    #[serde(default)]
    pub cache_unsafe_table_list: Vec<String>,
}

impl ResultCache {
    /// Environment variable holding the encryption key. Overrides
    /// `encryption_key` and `encryption_key_file`.
    pub const ENCRYPTION_KEY_ENV: &'static str = "PGDOG_RESULT_CACHE_ENCRYPTION_KEY";

    /// Settle `encryption_key` from `env_key` (the value of
    /// [`Self::ENCRYPTION_KEY_ENV`]), `encryption_key_file` or the
    /// configuration itself.
    pub fn resolve_encryption_key(&mut self, env_key: Option<String>) -> Result<(), Error> {
        if let Some(key) = env_key.filter(|key| !key.is_empty()) {
            self.encryption_key = Some(key);
            return Ok(());
        }

        if let Some(path) = &self.encryption_key_file {
            if self.encryption_key.is_some() {
                return Err(Error::ParseError(
                    "result_cache: set \"encryption_key\" or \"encryption_key_file\", not both"
                        .into(),
                ));
            }
            let key = read_to_string(path).map_err(|err| {
                Error::ParseError(format!(
                    "result_cache: \"encryption_key_file\" {}: {err}",
                    path.display()
                ))
            })?;
            self.encryption_key = Some(key.trim_end().to_string());
        }

        if self
            .encryption_key
            .as_deref()
            .is_some_and(|key| key.trim().is_empty())
        {
            return Err(Error::ParseError(
                "result_cache: the encryption key is empty".into(),
            ));
        }

        Ok(())
    }
}

/// How the result cache connects to Redis.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RedisMode {
    /// One Redis server at `redis_url`, or a proxy such as twemproxy or Envoy in front of several.
    #[default]
    Standalone,
    /// Redis Cluster, discovered from `redis_urls`.
    Cluster,
    /// Primary `redis_sentinel_master`, found through the Sentinels in `redis_urls`.
    Sentinel,
}

#[cfg(test)]
mod test {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    fn key_file(contents: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file
    }

    fn with_file(file: &NamedTempFile) -> ResultCache {
        ResultCache {
            encryption_key_file: Some(file.path().to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn test_key_is_read_from_file_without_trailing_newline() {
        let file = key_file("secret from file\n");
        let mut result_cache = with_file(&file);
        result_cache.resolve_encryption_key(None).unwrap();
        assert_eq!(
            result_cache.encryption_key.as_deref(),
            Some("secret from file")
        );
    }

    #[test]
    fn test_env_key_overrides_the_configuration() {
        let file = key_file("from file");
        let mut result_cache = with_file(&file);
        result_cache
            .resolve_encryption_key(Some("from env".into()))
            .unwrap();
        assert_eq!(result_cache.encryption_key.as_deref(), Some("from env"));

        let mut result_cache = ResultCache {
            encryption_key: Some("inline".into()),
            ..Default::default()
        };
        result_cache
            .resolve_encryption_key(Some("from env".into()))
            .unwrap();
        assert_eq!(result_cache.encryption_key.as_deref(), Some("from env"));
    }

    /// An empty variable, as container tooling often leaves them, doesn't count.
    #[test]
    fn test_empty_env_key_is_ignored() {
        let mut result_cache = ResultCache {
            encryption_key: Some("inline".into()),
            ..Default::default()
        };
        result_cache
            .resolve_encryption_key(Some("".into()))
            .unwrap();
        assert_eq!(result_cache.encryption_key.as_deref(), Some("inline"));
    }

    #[test]
    fn test_key_and_key_file_together_are_rejected() {
        let file = key_file("from file");
        let mut result_cache = ResultCache {
            encryption_key: Some("inline".into()),
            ..with_file(&file)
        };
        let err = result_cache.resolve_encryption_key(None).unwrap_err();
        assert!(err.to_string().contains("encryption_key_file"), "{err}");
    }

    #[test]
    fn test_unreadable_key_file_is_rejected() {
        let mut result_cache = ResultCache {
            encryption_key_file: Some("/nonexistent/pgdog.key".into()),
            ..Default::default()
        };
        let err = result_cache.resolve_encryption_key(None).unwrap_err();
        assert!(err.to_string().contains("/nonexistent/pgdog.key"), "{err}");
    }

    /// An empty key would encrypt with a key anyone can derive.
    #[test]
    fn test_empty_key_is_rejected() {
        let file = key_file(" \n");
        let mut from_file = with_file(&file);
        assert!(from_file.resolve_encryption_key(None).is_err());

        let mut inline = ResultCache {
            encryption_key: Some("".into()),
            ..Default::default()
        };
        assert!(inline.resolve_encryption_key(None).is_err());
    }

    #[test]
    fn test_no_key_configured_means_no_encryption() {
        let mut result_cache = ResultCache::default();
        result_cache.resolve_encryption_key(None).unwrap();
        assert_eq!(result_cache.encryption_key, None);
    }
}
