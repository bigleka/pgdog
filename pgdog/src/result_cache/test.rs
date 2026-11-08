use crate::{frontend::ClientRequest, net::messages::Query as QueryMsg, net::ProtocolMessage};

use super::CacheableRequest;

#[test]
fn cacheable_request_only_simple_select_read_route() {
    let mut req = ClientRequest::new();
    req.messages
        .push(ProtocolMessage::Query(QueryMsg::new("SELECT 1")));

    // Default route is write, so not cacheable.
    assert!(CacheableRequest::from_client_request(&req).is_none());
}

/// Redis-level behaviour, run against every supported Redis topology.
/// Require the topologies from the dev container (see `redis-topologies.sh`).
mod redis {
    use std::{
        ops::Deref,
        time::{Duration, SystemTime},
    };

    use pgdog_config::{RedisMode, ResultCache};
    use rand::{rng, Rng};

    use crate::{
        config::{set, ConfigAndUsers},
        frontend::router::parser::OwnedTable,
        net::Parameters,
        result_cache::{
            entry::Entry,
            lock::{Lock, Remote},
            redis::RedisResultCache,
            singleflight::Flight,
            CacheableRequest, ResultCacheKey,
        },
    };

    #[derive(Debug, Clone, Copy)]
    enum Topology {
        /// One Redis server.
        Standalone,
        /// Redis Cluster, 3 primaries.
        Cluster,
        /// twemproxy sharding over 2 Redis servers.
        Proxy,
        /// Primary found through Sentinel.
        Sentinel,
    }

    impl Topology {
        fn configure(self, result_cache: &mut ResultCache) {
            match self {
                Self::Standalone => result_cache.redis_url = "redis://127.0.0.1:6379".into(),
                Self::Proxy => result_cache.redis_url = "redis://127.0.0.1:22121".into(),
                Self::Sentinel => {
                    result_cache.redis_mode = RedisMode::Sentinel;
                    result_cache.redis_urls = vec!["redis://127.0.0.1:26379".into()];
                    result_cache.redis_sentinel_master = Some("pgdog-cache".into());
                }
                Self::Cluster => {
                    result_cache.redis_mode = RedisMode::Cluster;
                    result_cache.redis_urls = (7000..=7002)
                        .map(|port| format!("redis://127.0.0.1:{port}"))
                        .collect();
                }
            }
        }
    }

    async fn cache(topology: Topology) -> RedisResultCache {
        let mut result_cache = ResultCache {
            enabled: true,
            key_prefix: Some(format!("pgdog:test:{}", rng().random::<u64>())),
            ..Default::default()
        };
        topology.configure(&mut result_cache);

        let mut config = ConfigAndUsers::default();
        config.config.result_cache = Some(result_cache);
        set(config).unwrap();

        RedisResultCache::global()
            .await
            .unwrap_or_else(|| panic!("{topology:?} Redis reachable"))
    }

    /// One test per topology for each listed `async fn(Topology)`.
    macro_rules! for_each_topology {
        ($($case:ident),+ $(,)?) => {
            $(
                mod $case {
                    use super::Topology;

                    #[tokio::test]
                    async fn standalone() {
                        super::$case(Topology::Standalone).await;
                    }

                    #[tokio::test]
                    async fn cluster() {
                        super::$case(Topology::Cluster).await;
                    }

                    #[tokio::test]
                    async fn proxy() {
                        super::$case(Topology::Proxy).await;
                    }

                    #[tokio::test]
                    async fn sentinel() {
                        super::$case(Topology::Sentinel).await;
                    }
                }
            )+
        };
    }

    for_each_topology!(
        stored_result_is_served,
        store_after_invalidation_is_not_served,
        invalidation_drops_entries_reading_the_table,
        invalidation_keeps_entries_of_other_tables,
        lock_is_exclusive_between_instances,
        lock_is_released_only_by_its_owner,
        waiter_gets_result_of_lock_holder,
        waiter_leads_when_holder_gives_up,
        waiter_leads_when_lock_expires,
        lock_is_kept_while_holder_runs,
    );

    fn entry(payload: &[u8]) -> Entry {
        // Entries store milliseconds.
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        Entry {
            delta: Duration::from_millis(5),
            computed_at: SystemTime::UNIX_EPOCH + Duration::from_millis(now_ms as u64),
            payload: payload.to_vec(),
        }
    }

    fn table(name: &str) -> OwnedTable {
        OwnedTable {
            name: name.into(),
            ..Default::default()
        }
    }

    fn request(tables: &[OwnedTable]) -> CacheableRequest {
        let names: Vec<_> = tables.iter().map(|table| table.name.as_str()).collect();
        CacheableRequest {
            query: format!("SELECT * FROM {}", names.join(", ")),
            route_sig: "direct",
            tables: tables.to_vec(),
        }
    }

    /// Every client's QueryEngine gets its cache from `RedisResultCache::global()`.
    /// Singleflight only prevents a stampede if they share the in-flight queries.
    #[tokio::test]
    async fn test_singleflight_is_shared_between_clients() {
        cache(Topology::Standalone).await;
        let client_a = RedisResultCache::global().await.expect("redis reachable");
        let client_b = RedisResultCache::global().await.expect("redis reachable");

        let _leader = client_a.singleflight().join("same-key");

        assert!(
            matches!(
                client_b.singleflight().join("same-key"),
                Flight::Follower(_)
            ),
            "a second client missing on the same key should wait for the first one"
        );
    }

    async fn stored_result_is_served(topology: Topology) {
        let cache = cache(topology).await;
        let req = request(&[table("users")]);
        let params = Parameters::default();

        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        let stored = entry(b"fresh");
        cache
            .set_with_table_tags("db", &key, &stored, &req.tables)
            .await;

        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        let cached = cache.get(&key).await.expect("stored entry");
        assert_eq!(cached.entry, stored);
        assert!(
            !cached.refresh_early,
            "fresh entry of a fast query refreshed early"
        );
    }

    /// A read that started before a write and stores its result after the
    /// write's invalidation must not be served afterwards.
    async fn store_after_invalidation_is_not_served(topology: Topology) {
        let cache = cache(topology).await;
        let users = table("users");
        let req = request(std::slice::from_ref(&users));
        let params = Parameters::default();

        // Slow read: cache miss, key computed, query still running...
        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        assert!(cache.get(&key).await.is_none());

        // ...meanwhile a write commits and invalidates the table...
        cache.invalidate_tables("db", &[users]).await;

        // ...then the slow read stores its (now stale) result.
        cache
            .set_with_table_tags("db", &key, &entry(b"stale"), &req.tables)
            .await;

        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        assert!(cache.get(&key).await.is_none());
    }

    /// A query reading several tables (their keys can live on different
    /// cluster nodes) is invalidated by a write to any of them.
    async fn invalidation_drops_entries_reading_the_table(topology: Topology) {
        let cache = cache(topology).await;
        let tables = [table("users"), table("orders"), table("items")];
        let req = request(&tables);
        let params = Parameters::default();

        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        cache
            .set_with_table_tags("db", &key, &entry(b"joined"), &req.tables)
            .await;
        assert!(cache.get(&key).await.is_some(), "entry wasn't stored");

        cache.invalidate_tables("db", &[table("orders")]).await;

        assert!(
            cache.get(&key).await.is_none(),
            "tagged entry survived invalidation"
        );
        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        assert!(
            cache.get(&key).await.is_none(),
            "served under the new generation"
        );
    }

    async fn invalidation_keeps_entries_of_other_tables(topology: Topology) {
        let cache = cache(topology).await;
        let req = request(&[table("users")]);
        let params = Parameters::default();

        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        cache
            .set_with_table_tags("db", &key, &entry(b"users"), &req.tables)
            .await;

        cache
            .invalidate_tables("db", &[table("orders"), table("items")])
            .await;

        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        assert!(
            cache.get(&key).await.is_some(),
            "unrelated write dropped the entry"
        );
    }

    /// Another PgDog sharing the same Redis: its own connection and its
    /// own in-process singleflight.
    fn other_pgdog() -> RedisResultCache {
        RedisResultCache::default()
    }

    fn set_lock_ms(ms: u64) {
        let mut config = crate::config::config().deref().clone();
        if let Some(result_cache) = config.config.result_cache.as_mut() {
            result_cache.singleflight_lock_ms = Some(ms);
        }
        set(config).unwrap();
    }

    async fn some_key(cache: &RedisResultCache) -> ResultCacheKey {
        let req = request(&[table("users")]);
        cache
            .build_key("db", "user", &Parameters::default(), &req)
            .await
            .unwrap()
    }

    fn acquired(lock: Lock) -> crate::result_cache::lock::RemoteLock {
        match lock {
            Lock::Acquired(lock) => lock,
            other => panic!("expected to acquire the lock, got {other:?}"),
        }
    }

    async fn lock_is_exclusive_between_instances(topology: Topology) {
        let first = cache(topology).await;
        let second = other_pgdog();
        let key = some_key(&first).await;

        let lock = acquired(first.lock(&key).await);
        assert!(
            matches!(second.lock(&key).await, Lock::Held),
            "two PgDogs hold the lock on the same key"
        );

        lock.release().await;
        acquired(second.lock(&key).await);
    }

    /// A lock that expired (its holder couldn't renew it, e.g. cut off from
    /// Redis) and was taken by another PgDog isn't released by its former
    /// holder.
    async fn lock_is_released_only_by_its_owner(topology: Topology) {
        let first = cache(topology).await;
        set_lock_ms(200);
        let second = other_pgdog();
        let key = some_key(&first).await;

        let stale = acquired(first.lock(&key).await);
        stale.stop_renewing();
        tokio::time::sleep(Duration::from_millis(400)).await;
        let _current = acquired(second.lock(&key).await);

        stale.release().await;
        assert!(
            matches!(other_pgdog().lock(&key).await, Lock::Held),
            "former owner released someone else's lock"
        );
    }

    async fn waiter_gets_result_of_lock_holder(topology: Topology) {
        let holder = cache(topology).await;
        let waiter = other_pgdog();
        let key = some_key(&holder).await;
        let lock = acquired(holder.lock(&key).await);

        let waiting = {
            let key = key.clone();
            tokio::spawn(async move { waiter.lock_or_wait(&key).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "waiter didn't wait for the holder");

        let stored = entry(b"computed once");
        holder.set(&key, &stored).await;
        lock.release().await;

        match waiting.await.unwrap() {
            Remote::Served(cached) => assert_eq!(cached, stored),
            Remote::Lead(_) => panic!("waiter ran the query itself"),
        }
    }

    /// The holder failed (query error, client gone) and stored nothing.
    async fn waiter_leads_when_holder_gives_up(topology: Topology) {
        let holder = cache(topology).await;
        let waiter = other_pgdog();
        let key = some_key(&holder).await;
        let lock = acquired(holder.lock(&key).await);

        let waiting = {
            let key = key.clone();
            tokio::spawn(async move { waiter.lock_or_wait(&key).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(lock);

        let waited = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .expect("waiter hung after the holder gave up")
            .unwrap();
        assert!(
            matches!(waited, Remote::Lead(Some(_))),
            "waiter should take the lock over, got {waited:?}"
        );
    }

    /// The holder died without releasing: the lock expires and a waiter
    /// takes over.
    async fn waiter_leads_when_lock_expires(topology: Topology) {
        let holder = cache(topology).await;
        set_lock_ms(300);
        let waiter = other_pgdog();
        let key = some_key(&holder).await;
        acquired(holder.lock(&key).await).abandon();

        let start = std::time::Instant::now();
        let waited = tokio::time::timeout(Duration::from_secs(3), waiter.lock_or_wait(&key))
            .await
            .expect("waiter hung on an expired lock");
        assert!(
            matches!(waited, Remote::Lead(Some(_))),
            "waiter should take the lock over, got {waited:?}"
        );
        assert!(
            start.elapsed() >= Duration::from_millis(200),
            "waiter didn't wait for the lock to expire"
        );
    }

    /// A query running longer than the lock's expiry keeps the lock: other
    /// PgDogs keep waiting instead of running it too.
    async fn lock_is_kept_while_holder_runs(topology: Topology) {
        let holder = cache(topology).await;
        set_lock_ms(300);
        let key = some_key(&holder).await;
        let lock = acquired(holder.lock(&key).await);

        tokio::time::sleep(Duration::from_millis(1000)).await;
        let other = other_pgdog().lock(&key).await;
        lock.release().await;

        assert!(
            matches!(other, Lock::Held),
            "lock expired while its holder was still running, got {other:?}"
        );
    }
}
