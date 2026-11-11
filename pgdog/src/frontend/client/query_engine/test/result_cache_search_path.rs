//! A table named without a schema is whichever one the session's
//! search_path finds. Writes through either name must drop cached reads
//! through the other. Require Redis on 127.0.0.1:6379.

use pgdog_config::QueryParserLevel;
use rand::{rng, Rng};

use crate::backend::{server::test::test_server, Server};

use super::{
    prelude::*,
    result_cache::{enable_result_cache, select_value, simple},
};

/// Schema holding the table `t (id INT PRIMARY KEY, v INT)` with the row
/// `(1, 1)`, created directly in Postgres.
struct TestSchema {
    name: String,
    direct: Server,
}

impl TestSchema {
    async fn new() -> Self {
        let name = format!("result_cache_sp_{}", rng().random::<u32>());
        let mut direct = test_server().await;
        for statement in [
            format!("CREATE SCHEMA {name}"),
            format!("CREATE TABLE {name}.t (id INT PRIMARY KEY, v INT)"),
            format!("INSERT INTO {name}.t VALUES (1, 1)"),
        ] {
            direct.execute(statement).await.unwrap();
        }
        Self { name, direct }
    }

    /// Write that bypasses PgDog, so it can't invalidate anything.
    async fn update_directly(&mut self, value: i32) {
        self.direct
            .execute(format!(
                "UPDATE {}.t SET v = {value} WHERE id = 1",
                self.name
            ))
            .await
            .unwrap();
    }

    /// Client whose search_path is this schema.
    async fn client(&self) -> TestClient {
        let mut client = TestClient::new(Parameters::default()).await;
        simple(&mut client, &format!("SET search_path TO {}", self.name)).await;
        client
    }

    /// Cache `select` for `client` and prove later reads are cache hits.
    async fn prime(&mut self, client: &mut TestClient, select: &str) {
        assert_eq!(select_value(client, select).await.as_deref(), Some("1"));
        self.update_directly(1000).await;
        assert_eq!(
            select_value(client, select).await.as_deref(),
            Some("1"),
            "precondition: query should be served from the cache"
        );
        self.update_directly(1).await;
    }

    async fn drop(mut self) {
        self.direct
            .execute(format!("DROP SCHEMA {} CASCADE", self.name))
            .await
            .unwrap();
    }
}

/// `UPDATE t` by a session whose search_path finds `<schema>.t` drops the
/// cached `SELECT ... FROM <schema>.t`.
#[tokio::test]
async fn test_unqualified_write_invalidates_qualified_read() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut schema = TestSchema::new().await;
    let mut reader = TestClient::new(Parameters::default()).await;
    let mut writer = schema.client().await;
    let select = format!("SELECT v FROM {}.t WHERE id = 1", schema.name);
    schema.prime(&mut reader, &select).await;

    simple(&mut writer, "UPDATE t SET v = 2 WHERE id = 1").await;
    let after = select_value(&mut reader, &select).await;
    schema.drop().await;

    assert_eq!(
        after.as_deref(),
        Some("2"),
        "stale value served after UPDATE t"
    );
}

/// `UPDATE <schema>.t` drops the cached `SELECT ... FROM t` of a session
/// whose search_path finds `<schema>.t`.
#[tokio::test]
async fn test_qualified_write_invalidates_unqualified_read() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut schema = TestSchema::new().await;
    let mut reader = schema.client().await;
    let mut writer = TestClient::new(Parameters::default()).await;
    let select = "SELECT v FROM t WHERE id = 1";
    schema.prime(&mut reader, select).await;

    simple(
        &mut writer,
        &format!("UPDATE {}.t SET v = 2 WHERE id = 1", schema.name),
    )
    .await;
    let after = select_value(&mut reader, select).await;
    schema.drop().await;

    assert_eq!(
        after.as_deref(),
        Some("2"),
        "stale value served after UPDATE <schema>.t"
    );
}

/// The same unqualified query from sessions whose search_path finds
/// different tables doesn't share a cached result.
#[tokio::test]
async fn test_search_path_separates_cached_results() {
    enable_result_cache(QueryParserLevel::Auto);
    let one = TestSchema::new().await;
    let mut two = TestSchema::new().await;
    two.update_directly(2).await;
    let select = "SELECT v FROM t WHERE id = 1";

    // Dropping a test client shuts the pools down: keep both.
    let mut first_client = one.client().await;
    let mut second_client = two.client().await;
    let first = select_value(&mut first_client, select).await;
    let second = select_value(&mut second_client, select).await;
    one.drop().await;
    two.drop().await;

    assert_eq!(first.as_deref(), Some("1"));
    assert_eq!(second.as_deref(), Some("2"), "other search_path's result");
}
