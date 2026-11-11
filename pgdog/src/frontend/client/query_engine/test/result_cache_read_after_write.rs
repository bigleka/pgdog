//! Read-after-write with replicas, end to end. Require Redis on
//! 127.0.0.1:6379.
//!
//! The "replica" is another database on the same server holding the same
//! table: a replica that never catches up. A read that returns its value
//! went to the replica; one that returns the primary's went to the primary.

use std::time::Duration;

use pgdog_config::{Database, QueryParserLevel, ReadWriteSplit, ResultCache, Role};
use rand::{rng, Rng};
use tokio::time::sleep;

use crate::{
    backend::{
        databases::reload_from_existing, pool::Address, ConnectReason, Server, ServerOptions,
    },
    config::{set, ConfigAndUsers, User},
    frontend::router::parser::OwnedTable,
    result_cache::redis::RedisResultCache,
};

use super::{
    prelude::*,
    result_cache::{select_value, simple},
};

const WINDOW_MS: u64 = 1000;

/// Primary `pgdog`, replica `shard_3`, all reads sent to the replica.
fn configure(edit: impl FnOnce(&mut ResultCache)) {
    let mut config = ConfigAndUsers::default();
    config.config.databases = vec![
        Database {
            name: "pgdog".into(),
            host: "127.0.0.1".into(),
            port: 5432,
            role: Role::Primary,
            ..Default::default()
        },
        Database {
            name: "pgdog".into(),
            host: "127.0.0.1".into(),
            port: 5432,
            role: Role::Replica,
            database_name: Some("shard_3".into()),
            ..Default::default()
        },
    ];
    config.config.general.read_write_split = ReadWriteSplit::ExcludePrimary;
    config.config.general.query_parser = QueryParserLevel::Auto;
    config.users.users = vec![User {
        name: "pgdog".into(),
        database: "pgdog".into(),
        password: Some("pgdog".into()),
        ..Default::default()
    }];
    let mut result_cache = ResultCache {
        enabled: true,
        redis_url: "redis://127.0.0.1:6379".into(),
        key_prefix: Some(format!("pgdog:test:{}", rng().random::<u64>())),
        read_after_write_window_ms: Some(WINDOW_MS),
        ..Default::default()
    };
    edit(&mut result_cache);
    config.config.result_cache = Some(result_cache);
    set(config).unwrap();
    reload_from_existing().unwrap();
}

async fn connect(database: &str) -> Server {
    let address = Address {
        database_name: database.into(),
        ..Address::new_test()
    };
    Server::connect(&address, ServerOptions::default(), ConnectReason::Other)
        .await
        .unwrap()
}

/// Table `(id, v)` holding `(1, 1)` on both the primary and the replica.
struct Table {
    name: String,
    primary: Server,
    replica: Server,
}

impl Table {
    async fn new() -> Self {
        let name = format!("result_cache_raw_{}", rng().random::<u32>());
        let mut table = Self {
            name,
            primary: connect("pgdog").await,
            replica: connect("shard_3").await,
        };
        for server in [&mut table.primary, &mut table.replica] {
            server
                .execute(format!(
                    "CREATE TABLE {} (id INT PRIMARY KEY, v INT)",
                    table.name
                ))
                .await
                .unwrap();
            server
                .execute(format!("INSERT INTO {} VALUES (1, 1)", table.name))
                .await
                .unwrap();
        }
        table
    }

    fn select(&self) -> String {
        format!("SELECT v FROM {} WHERE id = 1", self.name)
    }

    fn update(&self, value: i32) -> String {
        format!("UPDATE {} SET v = {value} WHERE id = 1", self.name)
    }

    fn owned(&self) -> OwnedTable {
        OwnedTable {
            name: self.name.clone(),
            ..Default::default()
        }
    }

    async fn drop(mut self) {
        for server in [&mut self.primary, &mut self.replica] {
            server
                .execute(format!("DROP TABLE {}", self.name))
                .await
                .unwrap();
        }
    }
}

/// Without read-after-write, the replica's stale value is read, and cached
/// for every client: the bug this guards against.
#[tokio::test]
async fn test_disabled_reads_stale_replica() {
    configure(|result_cache| result_cache.read_after_write_window_ms = Some(0));
    let table = Table::new().await;
    let mut writer = TestClient::new(Parameters::default()).await;
    let mut reader = TestClient::new(Parameters::default()).await;

    simple(&mut writer, &table.update(2)).await;
    let value = select_value(&mut reader, &table.select()).await;
    table.drop().await;

    assert_eq!(value.as_deref(), Some("1"), "precondition: stale replica");
}

#[tokio::test]
async fn test_session_reads_its_write() {
    configure(|_| ());
    let table = Table::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    simple(&mut client, &table.update(2)).await;
    let value = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(
        value.as_deref(),
        Some("2"),
        "read went to the stale replica"
    );
    assert_eq!(primary_reads(), 1);
}

/// Each test runs in its own process: counters start at 0.
fn primary_reads() -> u64 {
    crate::stats::ResultCache::metrics()
        .into_iter()
        .map(|metric| metric.to_string())
        .find_map(|metric| {
            metric
                .lines()
                .find_map(|line| line.strip_prefix("result_cache_read_after_write_primary_reads "))
                .and_then(|value| value.parse().ok())
        })
        .unwrap()
}

/// Without replicas every read already goes to the primary: nothing to
/// count.
#[tokio::test]
async fn test_no_replicas_no_primary_reads_counted() {
    super::result_cache::enable_result_cache(QueryParserLevel::Auto);
    let table = super::result_cache::TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    simple(
        &mut client,
        &format!("UPDATE {} SET v = 2 WHERE id = 1", table.name()),
    )
    .await;
    let value = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(value.as_deref(), Some("2"));
    assert_eq!(primary_reads(), 0, "counted reads without replicas");
}

/// Another client right after the write reads from the primary, so the
/// result it caches is fresh.
#[tokio::test]
async fn test_other_client_reads_write_and_caches_it() {
    configure(|_| ());
    let mut table = Table::new().await;
    let mut writer = TestClient::new(Parameters::default()).await;
    let mut reader = TestClient::new(Parameters::default()).await;

    simple(&mut writer, &table.update(2)).await;
    let read = select_value(&mut reader, &table.select()).await;

    // Past the window, reads go to the replica again, but this one is
    // served from the cache, filled from the primary.
    sleep(Duration::from_millis(WINDOW_MS + 200)).await;
    table.primary.execute(table.update(3)).await.unwrap();
    let cached = select_value(&mut reader, &table.select()).await;
    table.drop().await;

    assert_eq!(read.as_deref(), Some("2"), "read went to the stale replica");
    assert_eq!(
        cached.as_deref(),
        Some("2"),
        "should be served from the cache"
    );
}

/// A write through another PgDog sharing Redis sends reads to the primary too.
#[tokio::test]
async fn test_write_through_other_pgdog() {
    configure(|_| ());
    let mut table = Table::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    table.primary.execute(table.update(2)).await.unwrap();
    RedisResultCache::default()
        .invalidate_tables("pgdog", &[table.owned()])
        .await;
    let value = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(
        value.as_deref(),
        Some("2"),
        "read went to the stale replica"
    );
}

/// Once the window passes, reads go back to the replicas.
#[tokio::test]
async fn test_replicas_serve_reads_after_window() {
    configure(|_| ());
    let table = Table::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    simple(&mut client, &table.update(2)).await;
    sleep(Duration::from_millis(WINDOW_MS + 200)).await;
    // Not cached yet: runs on a backend.
    let value = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(
        value.as_deref(),
        Some("1"),
        "read still went to the primary after the window"
    );
}

/// Reads PgDog doesn't cache still see the session's own writes.
#[tokio::test]
async fn test_session_reads_its_write_on_uncached_table() {
    let table = Table::new().await;
    let name = table.name.clone();
    configure(|result_cache| result_cache.cache_unsafe_table_list = vec![name]);
    let mut client = TestClient::new(Parameters::default()).await;

    simple(&mut client, &table.update(2)).await;
    let value = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(
        value.as_deref(),
        Some("2"),
        "read went to the stale replica"
    );
}

/// A transaction starting with a read of a table the session just wrote
/// runs on the primary.
#[tokio::test]
async fn test_transaction_reads_session_write() {
    configure(|_| ());
    let table = Table::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    simple(&mut client, &table.update(2)).await;
    simple(&mut client, "BEGIN").await;
    let value = select_value(&mut client, &table.select()).await;
    simple(&mut client, "COMMIT").await;
    table.drop().await;

    assert_eq!(
        value.as_deref(),
        Some("2"),
        "read went to the stale replica"
    );
}

/// Schema the table was created in, through the search_path.
async fn schema_of(table: &mut Table) -> String {
    table
        .primary
        .fetch_all::<String>(format!(
            "SELECT schemaname::text FROM pg_tables WHERE tablename = '{}'",
            table.name
        ))
        .await
        .unwrap()
        .pop()
        .expect("table exists")
}

/// `UPDATE t` then `SELECT ... FROM <schema>.t`, or the other way around:
/// the session's search_path can make them the same table, so the read must
/// see the write. Not cached, so only the session's own list sends it to
/// the primary.
async fn session_reads_its_write_by_other_name(qualified_write: bool) -> Option<String> {
    let mut table = Table::new().await;
    let name = table.name.clone();
    configure(|result_cache| result_cache.cache_unsafe_table_list = vec![name]);
    let schema = schema_of(&mut table).await;
    let qualified = format!("{schema}.{}", table.name);
    let (update, select) = if qualified_write {
        (
            format!("UPDATE {qualified} SET v = 2 WHERE id = 1"),
            table.select(),
        )
    } else {
        (
            table.update(2),
            format!("SELECT v FROM {qualified} WHERE id = 1"),
        )
    };
    let mut client = TestClient::new(Parameters::default()).await;

    simple(&mut client, &update).await;
    let value = select_value(&mut client, &select).await;
    table.drop().await;
    value
}

#[tokio::test]
async fn test_session_reads_unqualified_write_by_schema() {
    assert_eq!(
        session_reads_its_write_by_other_name(false)
            .await
            .as_deref(),
        Some("2"),
        "SELECT FROM <schema>.t after UPDATE t went to the stale replica"
    );
}

#[tokio::test]
async fn test_session_reads_qualified_write_unqualified() {
    assert_eq!(
        session_reads_its_write_by_other_name(true).await.as_deref(),
        Some("2"),
        "SELECT FROM t after UPDATE <schema>.t went to the stale replica"
    );
}

/// A read-only transaction already on the replica reads a table just
/// written: its stale result must not be cached for the clients reading
/// after it.
#[tokio::test]
async fn test_read_only_transaction_on_replica_doesnt_cache_stale_result() {
    configure(|result_cache| result_cache.cache_read_only_transactions = true);
    let table = Table::new().await;
    let mut writer = TestClient::new(Parameters::default()).await;
    let mut reader = TestClient::new(Parameters::default()).await;
    let mut other = TestClient::new(Parameters::default()).await;

    simple(
        &mut reader,
        "BEGIN READ ONLY ISOLATION LEVEL READ COMMITTED",
    )
    .await;
    // Runs on the replica, which the transaction then stays on.
    simple(&mut reader, "SELECT 1").await;
    simple(&mut writer, &table.update(2)).await;
    select_value(&mut reader, &table.select()).await;
    simple(&mut reader, "COMMIT").await;
    let after = select_value(&mut other, &table.select()).await;
    table.drop().await;

    assert_eq!(
        after.as_deref(),
        Some("2"),
        "replica's stale result was cached in the transaction"
    );
}
