//! Singleflight between PgDogs sharing Redis, end to end. Require Redis on
//! 127.0.0.1:6379.

use std::{
    ops::Deref,
    time::{Duration, Instant},
};

use pgdog_config::QueryParserLevel;

use crate::{
    config::{config, set},
    result_cache::{
        ResultCacheKey,
        lock::{Lock, RemoteLock},
        redis::RedisResultCache,
    },
};

use super::{
    prelude::*,
    result_cache::{TestTable, enable_result_cache, select_value},
    result_cache_xfetch::set_xfetch_beta,
};

fn set_lock_ms(ms: u64) {
    let mut config = config().deref().clone();
    if let Some(result_cache) = config.config.result_cache.as_mut() {
        result_cache.singleflight_lock_ms = Some(ms);
    }
    set(config).unwrap();
}

pub(super) async fn redis() -> redis::aio::MultiplexedConnection {
    redis::Client::open("redis://127.0.0.1:6379")
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap()
}

pub(super) fn key_prefix() -> String {
    config()
        .config
        .result_cache
        .as_ref()
        .and_then(|result_cache| result_cache.key_prefix.clone())
        .unwrap()
}

/// Run `table.select()` through PgDog once and return the Redis key and the
/// raw bytes of the entry it stored. This test run's key prefix is unique,
/// so it's the only entry under it.
async fn cached_entry(client: &mut TestClient, table: &TestTable) -> (ResultCacheKey, Vec<u8>) {
    assert_eq!(
        select_value(client, &table.select()).await.as_deref(),
        Some("1")
    );
    let mut redis = redis().await;
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{}:v3:*", key_prefix()))
        .query_async(&mut redis)
        .await
        .unwrap();
    assert_eq!(keys.len(), 1, "expected one entry, got {keys:?}");
    let bytes: Vec<u8> = redis::cmd("GET")
        .arg(&keys[0])
        .query_async(&mut redis)
        .await
        .unwrap();
    let key = ResultCacheKey {
        redis_key: keys[0].clone(),
        ..Default::default()
    };
    (key, bytes)
}

async fn delete(key: &ResultCacheKey) {
    let _: () = redis::cmd("DEL")
        .arg(&key.redis_key)
        .query_async(&mut redis().await)
        .await
        .unwrap();
}

/// Another PgDog sharing this Redis starts computing `key`.
async fn other_pgdog_locks(key: &ResultCacheKey) -> RemoteLock {
    match RedisResultCache::default().lock(key).await {
        Lock::Acquired(lock) => lock,
        other => panic!("other PgDog couldn't lock: {other:?}"),
    }
}

/// While another PgDog computes a result, a miss waits for it instead of
/// running the same query.
#[tokio::test]
async fn test_miss_waits_for_other_pgdog() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let (key, bytes) = cached_entry(&mut client, &table).await;

    // Expired, and the row changed since: running the query returns 2.
    delete(&key).await;
    table.update_directly(2).await;

    let lock = other_pgdog_locks(&key).await;
    let select = table.select();
    let query = tokio::spawn(async move { select_value(&mut client, &select).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let waited = !query.is_finished();

    // The other PgDog stores what it computed.
    let _: () = redis::cmd("SET")
        .arg(&key.redis_key)
        .arg(bytes)
        .query_async(&mut redis().await)
        .await
        .unwrap();
    lock.release().await;
    let value = query.await.unwrap();
    table.drop().await;

    assert!(waited, "miss didn't wait for the other PgDog");
    assert_eq!(
        value.as_deref(),
        Some("1"),
        "miss should get the other PgDog's result"
    );
}

/// The other PgDog died mid-query: once its lock expires, the miss runs the
/// query itself.
#[tokio::test]
async fn test_miss_runs_query_when_other_pgdog_dies() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let (key, _) = cached_entry(&mut client, &table).await;
    delete(&key).await;
    table.update_directly(2).await;

    set_lock_ms(500);
    other_pgdog_locks(&key).await.abandon();
    let start = Instant::now();
    let value = tokio::time::timeout(
        Duration::from_secs(5),
        select_value(&mut client, &table.select()),
    )
    .await
    .expect("miss hung on a dead PgDog's lock");
    let elapsed = start.elapsed();
    table.drop().await;

    assert_eq!(value.as_deref(), Some("2"));
    assert!(
        elapsed >= Duration::from_millis(400),
        "miss didn't wait for the lock ({elapsed:?})"
    );
}

/// After storing its result, the PgDog that ran the query releases the
/// lock, so the next miss on the key doesn't wait for it to expire.
#[tokio::test]
async fn test_lock_released_after_store() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let (key, _) = cached_entry(&mut client, &table).await;
    table.drop().await;

    assert!(
        matches!(
            RedisResultCache::default().lock(&key).await,
            Lock::Acquired(_)
        ),
        "lock still held after the result was stored"
    );
}

/// With the lock disabled, PgDogs don't wait for each other.
#[tokio::test]
async fn test_zero_lock_ms_disables_waiting() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let (key, _) = cached_entry(&mut client, &table).await;
    delete(&key).await;
    table.update_directly(2).await;

    let _lock = other_pgdog_locks(&key).await;
    set_lock_ms(0);
    let value = tokio::time::timeout(
        Duration::from_millis(500),
        select_value(&mut client, &table.select()),
    )
    .await
    .expect("waited with the lock disabled");
    table.drop().await;

    assert_eq!(value.as_deref(), Some("2"));
}

/// XFetch picked this hit to refresh early, but another PgDog already is:
/// serve the cached entry instead of refreshing it again.
#[tokio::test]
async fn test_early_refresh_skipped_while_other_pgdog_refreshes() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let (key, _) = cached_entry(&mut client, &table).await;
    table.update_directly(2).await;

    let _lock = other_pgdog_locks(&key).await;
    set_xfetch_beta(1e9);
    let value = tokio::time::timeout(
        Duration::from_millis(500),
        select_value(&mut client, &table.select()),
    )
    .await
    .expect("hit waited for the other PgDog's refresh");
    table.drop().await;

    assert_eq!(
        value.as_deref(),
        Some("1"),
        "hit refreshed the entry the other PgDog is refreshing"
    );
}
