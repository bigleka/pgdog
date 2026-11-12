//! Coalesce misses between PgDogs sharing Redis.
//!
//! [`Singleflight`](super::singleflight::Singleflight) leaves one client per
//! PgDog running a query. Before running it, that client locks the key in
//! Redis. If another PgDog holds the lock, it waits for that PgDog's result
//! to show up in the cache instead of running the same query.

use std::time::Duration;

use rand::{Rng, rng};
use tokio::{runtime::Handle, task::JoinHandle, time::sleep};
use tracing::warn;

use crate::stats::ResultCache as Stats;

use super::{
    ResultCacheKey,
    connection::Connection,
    entry::Entry,
    redis::{RedisResultCache, ResultCacheConfig},
};

/// How often a waiting PgDog checks for the result.
const POLL: Duration = Duration::from_millis(20);

/// Delete the lock only if it's still ours: once it expired, it may belong
/// to another PgDog.
///
/// Sent with EVAL every time: proxies like twemproxy don't support SCRIPT
/// LOAD, which EVALSHA falls back on.
const RELEASE: &str = r#"if redis.call("GET", KEYS[1]) == ARGV[1] then
    return redis.call("DEL", KEYS[1])
end
return 0"#;

/// Push the lock's expiry back, if it's still ours.
const RENEW: &str = r#"if redis.call("GET", KEYS[1]) == ARGV[1] then
    return redis.call("PEXPIRE", KEYS[1], ARGV[2])
end
return 0"#;

/// Outcome of trying to lock a key.
#[derive(Debug)]
pub enum Lock {
    /// This PgDog runs the query.
    Acquired(RemoteLock),
    /// Another PgDog is running it.
    Held,
    /// No lock: it's disabled or Redis is unavailable. Run the query.
    Unavailable,
}

/// What to do after a miss.
#[derive(Debug)]
pub enum Remote {
    /// Run the query, holding the lock if there is one.
    Lead(Option<RemoteLock>),
    /// Another PgDog ran it: serve its result.
    Served(Entry),
}

/// Lock on a key in Redis, held while this PgDog computes its result.
///
/// Renewed while held, so a query running longer than the lock's expiry
/// keeps it; the expiry only tells how soon other PgDogs take over if this
/// one dies. Released when dropped, so a failed query lets them run it
/// right away.
#[derive(Debug)]
pub struct RemoteLock {
    key: String,
    token: String,
    conn: Option<Connection>,
    renewal: JoinHandle<()>,
}

impl RemoteLock {
    /// Act as if this PgDog lost Redis: the lock expires, but it still holds it.
    #[cfg(test)]
    pub fn stop_renewing(&self) {
        self.renewal.abort();
    }

    /// Act as if this PgDog died: stop renewing the lock, don't release it.
    #[cfg(test)]
    pub fn abandon(mut self) {
        self.renewal.abort();
        self.conn = None;
    }

    /// Release the lock. Call after the result is stored, so PgDogs waiting
    /// for it find it in the cache.
    pub async fn release(mut self) {
        self.renewal.abort();
        if let Some(conn) = self.conn.take() {
            release(conn, &self.key, &self.token).await;
        }
    }
}

impl Drop for RemoteLock {
    fn drop(&mut self) {
        self.renewal.abort();
        let Some(conn) = self.conn.take() else {
            return;
        };
        if let Ok(handle) = Handle::try_current() {
            let key = std::mem::take(&mut self.key);
            let token = std::mem::take(&mut self.token);
            handle.spawn(async move { release(conn, &key, &token).await });
        }
    }
}

async fn release(mut conn: Connection, key: &str, token: &str) {
    let released: redis::RedisResult<i64> = redis::cmd("EVAL")
        .arg(RELEASE)
        .arg(1)
        .arg(key)
        .arg(token)
        .query_async(&mut conn)
        .await;
    if let Err(err) = released {
        Stats::redis_error();
        warn!("result_cache lock release failed: {}", err);
    }
}

/// Keep renewing the lock until aborted or it's no longer ours.
fn renew(mut conn: Connection, key: String, token: String, expiry: Duration) -> JoinHandle<()> {
    let expiry_ms = u64::try_from(expiry.as_millis()).unwrap_or(u64::MAX);
    tokio::spawn(async move {
        loop {
            sleep(expiry / 3).await;
            let renewed: redis::RedisResult<i64> = redis::cmd("EVAL")
                .arg(RENEW)
                .arg(1)
                .arg(&key)
                .arg(&token)
                .arg(expiry_ms)
                .query_async(&mut conn)
                .await;
            match renewed {
                Ok(1) => (),
                // Expired and maybe taken by another PgDog.
                Ok(_) => break,
                // Try again next time, while the lock hasn't expired.
                Err(err) => {
                    Stats::redis_error();
                    warn!("result_cache lock renewal failed: {}", err);
                }
            }
        }
    })
}

fn lock_key(key: &ResultCacheKey) -> String {
    format!("{}:lock", key.redis_key)
}

impl RedisResultCache {
    /// Try to lock `key` for this PgDog to compute its result.
    pub async fn lock(&self, key: &ResultCacheKey) -> Lock {
        let Some(cfg) = ResultCacheConfig::from_global_config() else {
            return Lock::Unavailable;
        };
        let Some(expiry) = cfg.singleflight_lock else {
            return Lock::Unavailable;
        };
        if self.ensure_connected(&cfg).await.is_none() {
            return Lock::Unavailable;
        }
        let Some(mut conn) = self.connection() else {
            return Lock::Unavailable;
        };

        let lock_key = lock_key(key);
        let token = format!("{:032x}", rng().random::<u128>());
        let locked: redis::RedisResult<Option<String>> = redis::cmd("SET")
            .arg(&lock_key)
            .arg(&token)
            .arg("NX")
            .arg("PX")
            .arg(u64::try_from(expiry.as_millis()).unwrap_or(u64::MAX))
            .query_async(&mut conn)
            .await;

        match locked {
            Ok(Some(_)) => Lock::Acquired(RemoteLock {
                renewal: renew(conn.clone(), lock_key.clone(), token.clone(), expiry),
                key: lock_key,
                token,
                conn: Some(conn),
            }),
            Ok(None) => Lock::Held,
            Err(err) => {
                Stats::redis_error();
                self.unavailable(&err);
                warn!("result_cache lock failed: {}", err);
                Lock::Unavailable
            }
        }
    }

    /// After a miss: lock `key`, or wait for the PgDog holding the lock to
    /// store its result. Takes the lock over if that PgDog gives up or dies.
    ///
    /// Waits as long as the query runs there, like clients waiting on a
    /// query in flight in this PgDog.
    pub async fn lock_or_wait(&self, key: &ResultCacheKey) -> Remote {
        match self.lock(key).await {
            Lock::Acquired(lock) => return Remote::Lead(Some(lock)),
            Lock::Unavailable => return Remote::Lead(None),
            Lock::Held => Stats::remote_wait(),
        }

        loop {
            sleep(POLL).await;
            if let Some(cached) = self.get(key).await {
                return Remote::Served(cached.entry);
            }
            match self.lock(key).await {
                Lock::Acquired(lock) => return Remote::Lead(Some(lock)),
                Lock::Unavailable => return Remote::Lead(None),
                Lock::Held => (),
            }
        }
    }
}
