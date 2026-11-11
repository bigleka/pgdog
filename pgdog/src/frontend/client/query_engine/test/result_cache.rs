//! Result cache tests. Require Redis on 127.0.0.1:6379.

use std::{ops::Deref, time::Duration};

use futures::future::join_all;

use pgdog_config::{QueryParserLevel, ResultCache};
use rand::{rng, Rng};

use crate::{
    backend::{databases::reload_from_existing, server::test::test_server, Server},
    config::{config, load_test, load_test_sharded, set},
    net::{DataRow, Message, ReadyForQuery},
};

use super::prelude::*;

const REDIS_URL: &str = "redis://127.0.0.1:6379";

/// Enable the result cache, using a key prefix unique to this test run.
pub(super) fn enable_result_cache(query_parser: QueryParserLevel) {
    load_test();
    add_result_cache(query_parser);
}

/// Enable the result cache in the loaded test config.
fn add_result_cache(query_parser: QueryParserLevel) {
    let mut config = config().deref().clone();
    config.config.general.query_parser = query_parser;
    config.config.result_cache = Some(ResultCache {
        enabled: true,
        redis_url: REDIS_URL.into(),
        key_prefix: Some(format!("pgdog:test:{}", rng().random::<u64>())),
        ..Default::default()
    });
    set(config).unwrap();
    reload_from_existing().unwrap();
}

/// Client with the result cache enabled, using a key prefix unique to this test run.
async fn result_cache_client(query_parser: QueryParserLevel) -> TestClient {
    enable_result_cache(query_parser);
    TestClient::new(Parameters::default()).await
}

pub(super) async fn simple(client: &mut TestClient, query: &str) -> Vec<Message> {
    client.send_simple(Query::new(query)).await;
    client.read_until('Z').await.unwrap()
}

/// Value of the first column of the first row returned by `query`.
pub(super) async fn select_value(client: &mut TestClient, query: &str) -> Option<String> {
    simple(client, query)
        .await
        .into_iter()
        .find(|message| message.code() == 'D')
        .and_then(|message| DataRow::try_from(message).ok())
        .and_then(|row| row.get_text(0))
}

#[tokio::test]
async fn test_write_in_transaction_invalidates_cache() {
    let table = format!("result_cache_tx_{}", rng().random::<u32>());
    let select = format!("SELECT v FROM {table} WHERE id = 1");

    let mut client = result_cache_client(QueryParserLevel::On).await;
    simple(
        &mut client,
        &format!("CREATE TABLE {table} (id INT PRIMARY KEY, v INT)"),
    )
    .await;
    simple(&mut client, &format!("INSERT INTO {table} VALUES (1, 1)")).await;

    // Populate the cache.
    assert_eq!(
        select_value(&mut client, &select).await.as_deref(),
        Some("1")
    );

    // A write PgDog doesn't see leaves the cached entry in place,
    // which proves the next read is served from the cache.
    let mut direct = test_server().await;
    direct
        .execute(format!("UPDATE {table} SET v = 2 WHERE id = 1"))
        .await
        .unwrap();
    assert_eq!(
        select_value(&mut client, &select).await.as_deref(),
        Some("1")
    );

    // A write inside a transaction must invalidate the entry once committed.
    simple(&mut client, "BEGIN").await;
    simple(
        &mut client,
        &format!("UPDATE {table} SET v = 3 WHERE id = 1"),
    )
    .await;
    simple(&mut client, "COMMIT").await;

    let value = select_value(&mut client, &select).await;
    direct.execute(format!("DROP TABLE {table}")).await.unwrap();

    assert_eq!(value.as_deref(), Some("3"));
}

#[tokio::test]
async fn test_result_cache_works_with_auto_query_parser() {
    let table = format!("result_cache_auto_{}", rng().random::<u32>());
    let select = format!("SELECT v FROM {table} WHERE id = 1");

    // Single primary: the query parser isn't needed for routing.
    let mut client = result_cache_client(QueryParserLevel::Auto).await;
    simple(
        &mut client,
        &format!("CREATE TABLE {table} (id INT PRIMARY KEY, v INT)"),
    )
    .await;
    simple(&mut client, &format!("INSERT INTO {table} VALUES (1, 1)")).await;

    assert_eq!(
        select_value(&mut client, &select).await.as_deref(),
        Some("1")
    );

    let mut direct = test_server().await;
    direct
        .execute(format!("UPDATE {table} SET v = 2 WHERE id = 1"))
        .await
        .unwrap();

    // Served from the cache.
    let value = select_value(&mut client, &select).await;
    direct.execute(format!("DROP TABLE {table}")).await.unwrap();

    assert_eq!(value.as_deref(), Some("1"));
}

#[tokio::test]
async fn test_pending_invalidations_dedup_until_commit() {
    let table = format!("result_cache_dedup_{}", rng().random::<u32>());

    let mut client = result_cache_client(QueryParserLevel::On).await;
    simple(
        &mut client,
        &format!("CREATE TABLE {table} (id INT PRIMARY KEY, v INT)"),
    )
    .await;

    simple(&mut client, "BEGIN").await;
    simple(&mut client, &format!("INSERT INTO {table} VALUES (1, 1)")).await;
    simple(
        &mut client,
        &format!("UPDATE {table} AS t SET v = 2 WHERE t.id = 1"),
    )
    .await;
    let pending = client.engine.pending_invalidations.len();
    simple(&mut client, "COMMIT").await;
    let pending_after_commit = client.engine.pending_invalidations.len();

    let mut direct = test_server().await;
    direct.execute(format!("DROP TABLE {table}")).await.unwrap();

    assert_eq!(pending, 1);
    assert_eq!(pending_after_commit, 0);
}

/// Cache stampede: many clients miss on the same key at once. Only one of
/// them should run the query; the others wait for its result.
#[tokio::test]
async fn test_concurrent_misses_run_query_once() {
    const CLIENTS: usize = 8;
    let suffix = rng().random::<u32>();
    let hits = format!("result_cache_hits_{suffix}");
    let function = format!("result_cache_slow_{suffix}");

    let mut direct = test_server().await;
    direct
        .execute(format!("CREATE TABLE {hits} (id SERIAL)"))
        .await
        .unwrap();
    // Records every execution and takes long enough for all clients to miss.
    direct
        .execute(format!(
            "CREATE FUNCTION {function}() RETURNS INT LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO {hits} DEFAULT VALUES; PERFORM pg_sleep(0.5); RETURN 42; END $$"
        ))
        .await
        .unwrap();

    enable_result_cache(QueryParserLevel::On);
    let mut clients = Vec::with_capacity(CLIENTS);
    for _ in 0..CLIENTS {
        clients.push(TestClient::new(Parameters::default()).await);
    }

    let select = format!("SELECT {function}()");
    let values = join_all(
        clients
            .iter_mut()
            .map(|client| select_value(client, &select)),
    )
    .await;

    let executions = direct
        .execute(format!("SELECT count(*) FROM {hits}"))
        .await
        .unwrap()
        .into_iter()
        .find(|message| message.code() == 'D')
        .and_then(|message| DataRow::try_from(message).ok())
        .and_then(|row| row.get_int(0, true));
    direct
        .execute(format!("DROP FUNCTION {function}(); DROP TABLE {hits}"))
        .await
        .unwrap();

    assert!(values.iter().all(|value| value.as_deref() == Some("42")));
    assert_eq!(
        executions,
        Some(1),
        "query should run once, not once per client"
    );
}

/// When the leader's query fails there's nothing to share: every waiting
/// client must get its own reply instead of hanging.
#[tokio::test]
async fn test_concurrent_misses_on_failing_query_dont_hang() {
    const CLIENTS: usize = 4;
    let function = format!("result_cache_fail_{}", rng().random::<u32>());

    let mut direct = test_server().await;
    direct
        .execute(format!(
            "CREATE FUNCTION {function}() RETURNS INT LANGUAGE plpgsql AS $$
             BEGIN PERFORM pg_sleep(0.3); RAISE EXCEPTION 'boom'; END $$"
        ))
        .await
        .unwrap();

    enable_result_cache(QueryParserLevel::On);
    let mut clients = Vec::with_capacity(CLIENTS);
    for _ in 0..CLIENTS {
        clients.push(TestClient::new(Parameters::default()).await);
    }

    let select = format!("SELECT {function}()");
    let replies = tokio::time::timeout(
        Duration::from_secs(10),
        join_all(clients.iter_mut().map(|client| async {
            client.send(Query::new(select.as_str())).await;
            client.try_process().await.unwrap();
            client.read_until('Z').await
        })),
    )
    .await;

    direct
        .execute(format!("DROP FUNCTION {function}()"))
        .await
        .unwrap();

    let replies = replies.expect("clients waiting on a failed leader hung");
    assert!(
        replies.iter().all(|reply| reply.is_err()),
        "every client should get the error"
    );
}

/// Table `(id INT PRIMARY KEY, v INT)` holding the row `(1, 1)`, created directly in Postgres.
pub(super) struct TestTable {
    name: String,
    direct: Server,
}

impl TestTable {
    pub(super) async fn new() -> Self {
        let name = format!("result_cache_{}", rng().random::<u32>());
        let mut direct = test_server().await;
        direct
            .execute(format!("CREATE TABLE {name} (id INT PRIMARY KEY, v INT)"))
            .await
            .unwrap();
        direct
            .execute(format!("INSERT INTO {name} VALUES (1, 1)"))
            .await
            .unwrap();
        Self { name, direct }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn select(&self) -> String {
        format!("SELECT v FROM {} WHERE id = 1", self.name)
    }

    /// Write that bypasses PgDog, so it can't invalidate anything.
    pub(super) async fn update_directly(&mut self, value: i32) {
        self.direct
            .execute(format!("UPDATE {} SET v = {value} WHERE id = 1", self.name))
            .await
            .unwrap();
    }

    pub(super) async fn drop(mut self) {
        self.direct
            .execute(format!("DROP TABLE {}", self.name))
            .await
            .unwrap();
    }
}

/// Fill the cache for `table.select()` and prove later reads are cache hits.
pub(super) async fn prime_cache(client: &mut TestClient, table: &mut TestTable) {
    assert_eq!(
        select_value(client, &table.select()).await.as_deref(),
        Some("1")
    );
    table.update_directly(1000).await;
    assert_eq!(
        select_value(client, &table.select()).await.as_deref(),
        Some("1"),
        "precondition: query should be served from the cache"
    );
    table.update_directly(1).await;
}

/// A cached reply replays the ReadyForQuery it was captured with. Inside a
/// transaction it must still tell the client it's in a transaction.
#[tokio::test]
async fn test_hit_inside_transaction_reports_transaction_status() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    prime_cache(&mut client, &mut table).await;

    table.update_directly(1000).await;
    simple(&mut client, "BEGIN READ ONLY ISOLATION LEVEL READ COMMITTED").await;
    let reply = simple(&mut client, &table.select()).await;
    simple(&mut client, "ROLLBACK").await;
    table.drop().await;

    let value = reply
        .iter()
        .find(|message| message.code() == 'D')
        .and_then(|message| DataRow::try_from(message.clone()).ok())
        .and_then(|row| row.get_text(0));
    assert_eq!(value.as_deref(), Some("1"), "precondition: a cache hit");
    let rfq = ReadyForQuery::try_from(reply.last().unwrap().clone()).unwrap();
    assert_eq!(
        rfq.status, 'T',
        "client was told it's idle while in a transaction"
    );
}

/// Rows written by an uncommitted (later rolled back) transaction must never
/// reach other sessions through the cache.
#[tokio::test]
async fn test_uncommitted_write_not_served_to_other_sessions() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut writer = TestClient::new(Parameters::default()).await;
    let mut reader = TestClient::new(Parameters::default()).await;

    simple(&mut writer, "BEGIN").await;
    simple(
        &mut writer,
        &format!("UPDATE {} SET v = 2 WHERE id = 1", table.name),
    )
    .await;
    assert_eq!(
        select_value(&mut writer, &table.select()).await.as_deref(),
        Some("2")
    );

    let during = select_value(&mut reader, &table.select()).await;
    simple(&mut writer, "ROLLBACK").await;
    let after_rollback = select_value(&mut reader, &table.select()).await;
    table.drop().await;

    assert_eq!(
        during.as_deref(),
        Some("1"),
        "uncommitted row served to another session"
    );
    assert_eq!(
        after_rollback.as_deref(),
        Some("1"),
        "rolled back row served after ROLLBACK"
    );
}

/// Another session reading while a write transaction is open caches the old
/// (still correct) value. COMMIT must invalidate it.
#[tokio::test]
async fn test_commit_invalidates_reads_cached_during_transaction() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut writer = TestClient::new(Parameters::default()).await;
    let mut reader = TestClient::new(Parameters::default()).await;

    simple(&mut writer, "BEGIN").await;
    simple(
        &mut writer,
        &format!("UPDATE {} SET v = 2 WHERE id = 1", table.name),
    )
    .await;
    assert_eq!(
        select_value(&mut reader, &table.select()).await.as_deref(),
        Some("1")
    );
    simple(&mut writer, "COMMIT").await;

    let after_commit = select_value(&mut reader, &table.select()).await;
    table.drop().await;

    assert_eq!(
        after_commit.as_deref(),
        Some("2"),
        "stale value served after COMMIT"
    );
}

/// Writes that aren't INSERT/UPDATE/DELETE must also invalidate.
#[tokio::test]
async fn test_truncate_invalidates() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut writer = TestClient::new(Parameters::default()).await;
    let mut reader = TestClient::new(Parameters::default()).await;
    prime_cache(&mut reader, &mut table).await;

    simple(&mut writer, &format!("TRUNCATE {}", table.name)).await;
    let after = select_value(&mut reader, &table.select()).await;
    table.drop().await;

    assert_eq!(after, None, "rows served after TRUNCATE");
}

/// A cached extended-protocol reply must match the messages the client sent:
/// Bind+Execute+Sync gets no ParseComplete.
#[tokio::test]
async fn test_extended_hit_matches_request_messages() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let param = [Parameter {
        len: 1,
        data: "1".as_bytes().into(),
    }];

    client
        .send(Parse::named(
            "stmt",
            format!("SELECT v FROM {} WHERE id = $1", table.name),
        ))
        .await;
    client.send(Bind::new_params("stmt", &param)).await;
    client.send(Execute::new()).await;
    client.send(Sync).await;
    client.try_process().await.unwrap();
    client.read_until('Z').await.unwrap();

    client.send(Bind::new_params("stmt", &param)).await;
    client.send(Execute::new()).await;
    client.send(Sync).await;
    client.try_process().await.unwrap();
    let reply = client.read_until('Z').await.unwrap();
    table.drop().await;

    let codes: String = reply.iter().map(|message| message.code()).collect();
    assert_eq!(codes, "2DCZ", "reply doesn't match Bind+Execute+Sync");
}

/// Same query and parameters, different result format: the second client
/// asked for binary and must not get the cached text rows.
#[tokio::test]
async fn test_extended_hit_respects_result_format() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let query = format!("SELECT v FROM {} WHERE id = $1", table.name);
    let param = [Parameter {
        len: 1,
        data: "1".as_bytes().into(),
    }];

    for results in [&[0][..], &[1][..]] {
        client.send(Parse::named("stmt_fmt", &query)).await;
        client
            .send(Bind::new_params_codes_results(
                "stmt_fmt",
                &param,
                &[],
                results,
            ))
            .await;
        client.send(Execute::new()).await;
        client.send(Sync).await;
        client.try_process().await.unwrap();
        let reply = client.read_until('Z').await.unwrap();

        if results == [1] {
            table.drop().await;
            let row = reply
                .iter()
                .find(|message| message.code() == 'D')
                .and_then(|message| DataRow::try_from(message.clone()).ok())
                .unwrap();
            assert_eq!(
                row.column(0).unwrap().len(),
                4,
                "binary int4 is 4 bytes; got the cached text value"
            );
            return;
        }
    }
}

/// A simple query with several statements is routed by its first one: a
/// write after a read in it must still invalidate.
#[tokio::test]
async fn test_write_after_read_in_one_query_invalidates() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut writer = TestClient::new(Parameters::default()).await;
    let mut reader = TestClient::new(Parameters::default()).await;
    prime_cache(&mut reader, &mut table).await;

    simple(
        &mut writer,
        &format!("SELECT 1; UPDATE {} SET v = 2 WHERE id = 1", table.name),
    )
    .await;
    let after = select_value(&mut reader, &table.select()).await;
    table.drop().await;

    assert_eq!(after.as_deref(), Some("2"), "stale read after the write");
}

/// ... and isn't a read the cache can replay: the write must run each time,
/// even when nothing in the query names a table to invalidate.
#[tokio::test]
async fn test_query_with_write_after_read_not_cached() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    let query = format!(
        "SELECT 1; DO $$ BEGIN UPDATE {} SET v = v + 1 WHERE id = 1; END $$",
        table.name
    );
    simple(&mut client, &query).await;
    simple(&mut client, &query).await;
    let value = select_value(
        &mut client,
        &format!("SELECT v + 0 FROM {} WHERE id = 1", table.name),
    )
    .await;
    table.drop().await;

    assert_eq!(value.as_deref(), Some("3"), "second UPDATE didn't run");
}

/// A query split into several requests gets one ReadyForQuery, after the
/// last one: a cached reply, which ends with one, can't stand in for the
/// others.
#[tokio::test]
async fn test_split_query_not_served_from_cache() {
    load_test_sharded();
    add_result_cache(QueryParserLevel::Auto);
    let mut client = TestClient::new(Parameters::default()).await;

    for _ in 0..2 {
        simple(&mut client, "SELECT 1 AS cached; SET statement_timeout TO '10s'").await;
    }

    assert_eq!(
        select_value(&mut client, "SELECT 2").await.as_deref(),
        Some("2"),
        "client got an extra ReadyForQuery"
    );
}
