//! Result cache encryption (`encryption_key`), end to end. Require Redis on
//! 127.0.0.1:6379.

use std::ops::Deref;

use pgdog_config::QueryParserLevel;

use crate::config::{config, set};

use super::{
    prelude::*,
    result_cache::{TestTable, enable_result_cache, select_value},
    result_cache_distributed::{key_prefix, redis},
};

/// Text in every cached result; readable in Redis only without encryption.
const SECRET: &str = "pgdog-secret-7c1e";

fn set_encryption_key(key: Option<&str>) {
    let mut config = config().deref().clone();
    if let Some(result_cache) = config.config.result_cache.as_mut() {
        result_cache.encryption_key = key.map(String::from);
    }
    set(config).unwrap();
}

/// Returns `SECRET` followed by the table's value.
fn select_secret(table: &TestTable) -> String {
    format!("SELECT '{SECRET}' || v FROM {} WHERE id = 1", table.name())
}

/// Raw values of the result entries stored under this test run's prefix.
async fn stored_entries() -> Vec<Vec<u8>> {
    let mut redis = redis().await;
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{}:v3:*", key_prefix()))
        .query_async(&mut redis)
        .await
        .unwrap();
    let mut entries = vec![];
    for key in keys {
        let bytes: Vec<u8> = redis::cmd("GET")
            .arg(key)
            .query_async(&mut redis)
            .await
            .unwrap();
        entries.push(bytes);
    }
    entries
}

fn holds_secret(entry: &[u8]) -> bool {
    entry
        .windows(SECRET.len())
        .any(|window| window == SECRET.as_bytes())
}

/// Without a key, results are stored as they are; this is what the tests
/// below look for.
#[tokio::test]
async fn test_without_key_results_are_readable_in_redis() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    select_value(&mut client, &select_secret(&table)).await;
    let entries = stored_entries().await;
    table.drop().await;

    assert_eq!(entries.len(), 1);
    assert!(holds_secret(&entries[0]));
}

/// With a key, Redis holds no readable result, and hits still work.
#[tokio::test]
async fn test_with_key_results_are_encrypted_in_redis() {
    enable_result_cache(QueryParserLevel::Auto);
    set_encryption_key(Some("test key"));
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    let first = select_value(&mut client, &select_secret(&table)).await;
    let entries = stored_entries().await;
    table.update_directly(2).await;
    let hit = select_value(&mut client, &select_secret(&table)).await;
    table.drop().await;

    assert_eq!(first, Some(format!("{SECRET}1")));
    assert_eq!(entries.len(), 1);
    assert!(
        !holds_secret(&entries[0]),
        "result stored in Redis in clear text"
    );
    assert_eq!(
        hit,
        Some(format!("{SECRET}1")),
        "encrypted entry not served"
    );
}

/// An entry written with another key can't be decrypted: the query goes to
/// Postgres instead of returning it.
#[tokio::test]
async fn test_entry_encrypted_with_another_key_is_not_served() {
    enable_result_cache(QueryParserLevel::Auto);
    set_encryption_key(Some("old key"));
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    select_value(&mut client, &select_secret(&table)).await;
    table.update_directly(2).await;
    set_encryption_key(Some("new key"));
    let after = select_value(&mut client, &select_secret(&table)).await;
    table.drop().await;

    assert_eq!(after, Some(format!("{SECRET}2")));
}

/// Entries stored before encryption was turned on aren't served either.
#[tokio::test]
async fn test_plaintext_entry_is_not_served_once_key_is_set() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    select_value(&mut client, &select_secret(&table)).await;
    table.update_directly(2).await;
    set_encryption_key(Some("new key"));
    let after = select_value(&mut client, &select_secret(&table)).await;
    table.drop().await;

    assert_eq!(after, Some(format!("{SECRET}2")));
}
