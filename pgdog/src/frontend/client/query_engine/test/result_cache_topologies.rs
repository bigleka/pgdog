//! The result cache through the query engine on each Redis topology.
//! Require the topologies from the dev container (see `redis-topologies.sh`).

use std::ops::Deref;

use pgdog_config::{QueryParserLevel, RedisMode};

use crate::config::{config, set};

use super::{
    prelude::*,
    result_cache::{enable_result_cache, select_value, simple, TestTable},
};

fn use_redis(mode: RedisMode, url: &str, urls: &[&str]) {
    enable_result_cache(QueryParserLevel::Auto);
    let mut config = config().deref().clone();
    if let Some(result_cache) = config.config.result_cache.as_mut() {
        result_cache.redis_mode = mode;
        result_cache.redis_url = url.into();
        result_cache.redis_urls = urls.iter().map(|url| url.to_string()).collect();
    }
    set(config).unwrap();
}

/// Cache hit, then a write through PgDog invalidates it.
async fn cache_and_invalidate() {
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );
    table.update_directly(2).await;
    let cached = select_value(&mut client, &table.select()).await;

    simple(
        &mut client,
        &format!("UPDATE {} SET v = 3 WHERE id = 1", table.name()),
    )
    .await;
    let after_write = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(
        cached.as_deref(),
        Some("1"),
        "second read wasn't a cache hit"
    );
    assert_eq!(
        after_write.as_deref(),
        Some("3"),
        "write didn't invalidate the entry"
    );
}

#[tokio::test]
async fn test_redis_cluster() {
    use_redis(
        RedisMode::Cluster,
        "",
        &[
            "redis://127.0.0.1:7000",
            "redis://127.0.0.1:7001",
            "redis://127.0.0.1:7002",
        ],
    );
    cache_and_invalidate().await;
}

/// Seed list of one node: the rest of the cluster is discovered.
#[tokio::test]
async fn test_redis_cluster_from_redis_url() {
    use_redis(RedisMode::Cluster, "redis://127.0.0.1:7001", &[]);
    cache_and_invalidate().await;
}

#[tokio::test]
async fn test_proxy() {
    use_redis(RedisMode::Standalone, "redis://127.0.0.1:22121", &[]);
    cache_and_invalidate().await;
}

/// With Redis down, the cache must get out of the way: queries go straight
/// to Postgres without waiting on Redis.
#[tokio::test]
async fn test_unreachable_redis_doesnt_slow_queries() {
    // Nothing listens on port 1.
    use_redis(RedisMode::Standalone, "redis://127.0.0.1:1", &[]);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    let start = std::time::Instant::now();
    for _ in 0..3 {
        assert_eq!(
            select_value(&mut client, &table.select()).await.as_deref(),
            Some("1")
        );
    }
    let elapsed = start.elapsed();
    table.drop().await;

    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "3 queries took {elapsed:?} with Redis down"
    );
}

/// A Redis that accepts connections but never answers (hung, overloaded, or
/// behind a network dropping packets) mustn't cost every query a timeout.
#[tokio::test]
async fn test_unresponsive_redis_doesnt_slow_queries() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // Accept and hold connections, never reply.
    let silent = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });

    use_redis(
        RedisMode::Standalone,
        &format!("redis://127.0.0.1:{port}"),
        &[],
    );
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    let start = std::time::Instant::now();
    for _ in 0..5 {
        assert_eq!(
            select_value(&mut client, &table.select()).await.as_deref(),
            Some("1")
        );
    }
    let elapsed = start.elapsed();
    table.drop().await;
    silent.abort();

    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "5 queries took {elapsed:?} with an unresponsive Redis"
    );
}

/// Once Redis is back, the cache resumes after the backoff.
#[tokio::test]
async fn test_cache_resumes_when_redis_returns() {
    // Reserve a port with nothing listening on it yet.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    use_redis(
        RedisMode::Standalone,
        &format!("redis://127.0.0.1:{port}"),
        &[],
    );
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    // Redis down: served by Postgres.
    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );

    // Redis comes back on the same address: forward it to the real one.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let forwarder = tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut outbound = tokio::net::TcpStream::connect("127.0.0.1:6379")
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            });
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(5500)).await;

    // Cached again: a change made outside PgDog stays hidden.
    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );
    table.update_directly(2).await;
    let value = select_value(&mut client, &table.select()).await;
    table.drop().await;
    forwarder.abort();

    assert_eq!(
        value.as_deref(),
        Some("1"),
        "cache didn't resume after Redis came back"
    );
}
