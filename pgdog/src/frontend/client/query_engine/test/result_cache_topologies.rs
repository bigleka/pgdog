//! The result cache through the query engine on each Redis topology.
//! Require the topologies from the dev container (see `redis-topologies.sh`).

use std::ops::Deref;

use pgdog_config::{QueryParserLevel, RedisMode};

use crate::config::{config, set};

use super::{
    prelude::*,
    result_cache::{TestTable, enable_result_cache, select_value, simple},
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
    Box::pin(cache_and_invalidate()).await;
}

/// Seed list of one node: the rest of the cluster is discovered.
#[tokio::test]
async fn test_redis_cluster_from_redis_url() {
    use_redis(RedisMode::Cluster, "redis://127.0.0.1:7001", &[]);
    Box::pin(cache_and_invalidate()).await;
}

#[tokio::test]
async fn test_proxy() {
    use_redis(RedisMode::Standalone, "redis://127.0.0.1:22121", &[]);
    Box::pin(cache_and_invalidate()).await;
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

fn use_sentinel(master: &str) {
    use_redis(RedisMode::Sentinel, "", &["redis://127.0.0.1:26379"]);
    let mut config = config().deref().clone();
    if let Some(result_cache) = config.config.result_cache.as_mut() {
        result_cache.redis_sentinel_master = Some(master.into());
    }
    set(config).unwrap();
}

#[tokio::test]
async fn test_redis_sentinel() {
    use_sentinel("pgdog-cache");
    Box::pin(cache_and_invalidate()).await;
}

/// After a Sentinel failover, PgDog moves to the new primary: writes through
/// PgDog invalidate there, and results are cached there again.
#[tokio::test]
async fn test_redis_sentinel_failover() {
    use_sentinel("pgdog-failover");
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    // Cached on the current primary.
    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );

    // Sentinel sometimes takes longer than the client's default timeout to
    // answer, e.g. while it's still settling after startup.
    let patient = redis::AsyncConnectionConfig::new()
        .set_response_timeout(Some(std::time::Duration::from_secs(5)));
    let mut sentinel = redis::Client::open("redis://127.0.0.1:26379")
        .unwrap()
        .get_multiplexed_async_connection_with_config(&patient)
        .await
        .unwrap();
    let primary = |sentinel: &mut redis::aio::MultiplexedConnection| {
        let mut sentinel = sentinel.clone();
        async move {
            redis::cmd("SENTINEL")
                .arg("get-master-addr-by-name")
                .arg("pgdog-failover")
                .query_async::<Vec<String>>(&mut sentinel)
                .await
                .unwrap()
        }
    };
    let before = primary(&mut sentinel).await;
    let _: () = redis::cmd("SENTINEL")
        .arg("failover")
        .arg("pgdog-failover")
        .query_async(&mut sentinel)
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while primary(&mut sentinel).await == before {
        assert!(
            std::time::Instant::now() < deadline,
            "Sentinel didn't fail over"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    // Wait for the former primary to actually turn into a replica: until then
    // it still accepts the writes of a client that didn't move. It drops its
    // clients when demoted, so ask on a fresh connection each time.
    let former = redis::Client::open(format!("redis://{}:{}", before[0], before[1])).unwrap();
    loop {
        let role = async {
            let mut conn = former.get_multiplexed_async_connection().await?;
            redis::cmd("ROLE")
                .query_async::<Vec<redis::Value>>(&mut conn)
                .await
        }
        .await;
        if let Ok(role) = role
            && matches!(role.first(), Some(redis::Value::BulkString(role)) if role == b"slave")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "former primary never became a replica"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    // Writes keep working through the switch; once reconnected, a write
    // invalidates on the new primary and the next read is cached there.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut version = 10;
    loop {
        version += 1;
        simple(
            &mut client,
            &format!("UPDATE {} SET v = {version} WHERE id = 1", table.name()),
        )
        .await;
        assert_eq!(
            select_value(&mut client, &table.select()).await,
            Some(version.to_string()),
            "stale value after a write through PgDog"
        );
        table.update_directly(0).await;
        if select_value(&mut client, &table.select()).await == Some(version.to_string()) {
            break; // Served from the cache on the new primary.
        }
        assert!(
            std::time::Instant::now() < deadline,
            "cache never resumed on the new primary"
        );
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    table.drop().await;
}

fn configure_redis(edit: impl FnOnce(&mut pgdog_config::ResultCache)) {
    let mut config = config().deref().clone();
    if let Some(result_cache) = config.config.result_cache.as_mut() {
        edit(result_cache);
    }
    set(config).unwrap();
}

/// A DBA can give a slow Redis more time before it counts as down.
#[tokio::test]
async fn test_response_timeout_is_configurable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
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
    configure_redis(|result_cache| result_cache.redis_response_timeout_ms = Some(1500));
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    let start = std::time::Instant::now();
    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );
    let elapsed = start.elapsed();
    table.drop().await;
    silent.abort();

    assert!(
        elapsed >= std::time::Duration::from_millis(1400),
        "gave up on Redis after {elapsed:?}, before the configured timeout"
    );
}

/// How long the cache is skipped after Redis fails is configurable.
#[tokio::test]
async fn test_unavailable_backoff_is_configurable() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    use_redis(
        RedisMode::Standalone,
        &format!("redis://127.0.0.1:{port}"),
        &[],
    );
    configure_redis(|result_cache| result_cache.redis_unavailable_backoff_ms = Some(200));
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    // Redis down.
    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );

    // Back on the same address, forwarded to the real one.
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
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

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
        "cache didn't resume once the configured backoff passed"
    );
}

/// Value of `result_cache_redis_up`.
fn redis_up() -> Option<String> {
    gauge("result_cache_redis_up")
}

fn gauge(name: &str) -> Option<String> {
    let prefix = format!("{name} ");
    crate::stats::ResultCache::metrics()
        .into_iter()
        .map(|metric| metric.to_string())
        .flat_map(|metric| metric.lines().map(str::to_owned).collect::<Vec<_>>())
        .find_map(|line| line.strip_prefix(prefix.as_str()).map(str::to_owned))
}

/// Tells whether the cache can use Redis right now, for alerting.
#[tokio::test]
async fn test_redis_up_metric() {
    use_redis(RedisMode::Standalone, "redis://127.0.0.1:6379", &[]);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    select_value(&mut client, &table.select()).await;
    let up = redis_up();

    use_redis(RedisMode::Standalone, "redis://127.0.0.1:1", &[]);
    select_value(&mut client, &table.select()).await;
    let down = redis_up();

    use_redis(RedisMode::Standalone, "redis://127.0.0.1:6379", &[]);
    select_value(&mut client, &table.select()).await;
    let back = redis_up();
    table.drop().await;

    assert_eq!(up.as_deref(), Some("1"));
    assert_eq!(gauge("result_cache_enabled").as_deref(), Some("1"));
    assert_eq!(down.as_deref(), Some("0"), "Redis unreachable");
    assert_eq!(back.as_deref(), Some("1"), "Redis reachable again");
}
