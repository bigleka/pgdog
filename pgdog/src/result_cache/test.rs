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

mod redis {
    //! Require Redis on 127.0.0.1:6379.

    use pgdog_config::ResultCache;
    use rand::{rng, Rng};

    use crate::{
        config::{set, ConfigAndUsers},
        frontend::router::parser::OwnedTable,
        net::Parameters,
        result_cache::{redis::RedisResultCache, singleflight::Flight, CacheableRequest},
    };

    async fn cache() -> RedisResultCache {
        let mut config = ConfigAndUsers::default();
        config.config.result_cache = Some(ResultCache {
            enabled: true,
            redis_url: "redis://127.0.0.1:6379".into(),
            key_prefix: Some(format!("pgdog:test:{}", rng().random::<u64>())),
            ..Default::default()
        });
        set(config).unwrap();

        RedisResultCache::global().await.expect("redis reachable")
    }

    fn request(table: &OwnedTable) -> CacheableRequest {
        CacheableRequest {
            query: format!("SELECT * FROM {}", table.name),
            route_sig: "direct",
            tables: vec![table.clone()],
        }
    }

    /// Every client's QueryEngine gets its cache from `RedisResultCache::global()`.
    /// Singleflight only prevents a stampede if they share the in-flight queries.
    #[tokio::test]
    async fn test_singleflight_is_shared_between_clients() {
        cache().await;
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

    /// A read that started before a write and stores its result after the
    /// write's invalidation must not be served afterwards.
    #[tokio::test]
    async fn test_store_after_invalidation_is_not_served() {
        let cache = cache().await;
        let table = OwnedTable {
            name: "users".into(),
            ..Default::default()
        };
        let params = Parameters::default();
        let req = request(&table);

        // Slow read: cache miss, key computed, query still running...
        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        assert!(cache.get(&key).await.is_none());

        // ...meanwhile a write commits and invalidates the table...
        cache.invalidate_tables("db", &[table.clone()]).await;

        // ...then the slow read stores its (now stale) result.
        cache
            .set_with_table_tags("db", &key, b"stale", &req.tables)
            .await;

        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        assert_eq!(cache.get(&key).await, None);
    }

    #[tokio::test]
    async fn test_stored_result_is_served() {
        let cache = cache().await;
        let table = OwnedTable {
            name: "users".into(),
            ..Default::default()
        };
        let params = Parameters::default();
        let req = request(&table);

        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        cache
            .set_with_table_tags("db", &key, b"fresh", &req.tables)
            .await;

        let key = cache.build_key("db", "user", &params, &req).await.unwrap();
        assert_eq!(cache.get(&key).await.as_deref(), Some(&b"fresh"[..]));
    }
}
