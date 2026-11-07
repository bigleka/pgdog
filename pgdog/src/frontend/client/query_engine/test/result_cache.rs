//! Result cache tests. Require Redis on 127.0.0.1:6379.

use std::ops::Deref;

use pgdog_config::{QueryParserLevel, ResultCache};
use rand::{rng, Rng};

use crate::{
    backend::{databases::reload_from_existing, server::test::test_server},
    config::{config, load_test, set},
    net::{DataRow, Message},
};

use super::prelude::*;

const REDIS_URL: &str = "redis://127.0.0.1:6379";

/// Client with the result cache enabled, using a key prefix unique to this test run.
async fn result_cache_client(query_parser: QueryParserLevel) -> TestClient {
    load_test();

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

    TestClient::new(Parameters::default()).await
}

async fn simple(client: &mut TestClient, query: &str) -> Vec<Message> {
    client.send_simple(Query::new(query)).await;
    client.read_until('Z').await.unwrap()
}

/// Value of the first column of the first row returned by `query`.
async fn select_value(client: &mut TestClient, query: &str) -> Option<String> {
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
