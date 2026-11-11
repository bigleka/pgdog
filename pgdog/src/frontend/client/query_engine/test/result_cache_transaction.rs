//! The result cache inside transactions. Require Redis on 127.0.0.1:6379.
//!
//! Only read-only `READ COMMITTED` transactions can use it: every statement
//! there takes a new snapshot anyway, so a cached result is as good as an
//! autocommit read. `REPEATABLE READ` and `SERIALIZABLE` statements must all
//! see the transaction's snapshot.

use std::ops::Deref;

use pgdog_config::QueryParserLevel;

use crate::{
    config::{config, set},
    net::{Message, ReadyForQuery},
};

use super::{
    prelude::*,
    result_cache::{enable_result_cache, prime_cache, select_value, simple, TestTable},
};

/// The cache, with `cache_read_only_transactions` set to `enabled`.
fn enable(cache_read_only_transactions: bool) {
    enable_result_cache(QueryParserLevel::Auto);
    let mut config = config().deref().clone();
    if let Some(result_cache) = config.config.result_cache.as_mut() {
        result_cache.cache_read_only_transactions = cache_read_only_transactions;
    }
    set(config).unwrap();
}

/// Value read by `table.select()` after running `statements` in a new
/// client, once the cache holds `1` and the table holds `1000`: `1` comes
/// from the cache, `1000` from Postgres.
async fn read_in_transaction(cache_read_only_transactions: bool, statements: &[&str]) -> String {
    enable(cache_read_only_transactions);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    prime_cache(&mut client, &mut table).await;
    table.update_directly(1000).await;

    for statement in statements {
        simple(&mut client, statement).await;
    }
    let value = select_value(&mut client, &table.select()).await;
    simple(&mut client, "COMMIT").await;
    table.drop().await;
    value.expect("a row")
}

const CACHED: &str = "1";
const FROM_POSTGRES: &str = "1000";

/// Every message of the reply to `query`, errors included.
async fn reply(client: &mut TestClient, query: &str) -> Vec<Message> {
    client.send_simple(Query::new(query)).await;
    let mut reply = vec![];
    loop {
        let message = client.read().await;
        let done = message.code() == 'Z';
        reply.push(message);
        if done {
            return reply;
        }
    }
}

fn status(reply: &[Message]) -> char {
    ReadyForQuery::try_from(reply.last().expect("a reply").clone())
        .expect("ends with ReadyForQuery")
        .status
}

#[tokio::test]
async fn test_read_only_read_committed_uses_cache() {
    let value =
        read_in_transaction(false, &["BEGIN READ ONLY ISOLATION LEVEL READ COMMITTED"]).await;
    assert_eq!(value, CACHED);
}

#[tokio::test]
async fn test_start_transaction_read_committed_read_only_uses_cache() {
    let value = read_in_transaction(
        false,
        &["START TRANSACTION ISOLATION LEVEL READ COMMITTED, READ ONLY"],
    )
    .await;
    assert_eq!(value, CACHED);
}

#[tokio::test]
async fn test_set_transaction_read_committed_uses_cache() {
    let value = read_in_transaction(
        false,
        &[
            "BEGIN READ ONLY",
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
        ],
    )
    .await;
    assert_eq!(value, CACHED);
}

/// The default isolation level can come from ALTER ROLE: only with the flag.
#[tokio::test]
async fn test_read_only_default_isolation_skips_cache() {
    let value = read_in_transaction(false, &["BEGIN READ ONLY"]).await;
    assert_eq!(value, FROM_POSTGRES);
}

#[tokio::test]
async fn test_read_only_default_isolation_uses_cache_with_flag() {
    let value = read_in_transaction(true, &["BEGIN READ ONLY"]).await;
    assert_eq!(value, CACHED);
}

#[tokio::test]
async fn test_read_write_transaction_skips_cache() {
    let value = read_in_transaction(true, &["BEGIN ISOLATION LEVEL READ COMMITTED"]).await;
    assert_eq!(value, FROM_POSTGRES);
}

#[tokio::test]
async fn test_repeatable_read_skips_cache() {
    let value =
        read_in_transaction(true, &["BEGIN READ ONLY ISOLATION LEVEL REPEATABLE READ"]).await;
    assert_eq!(value, FROM_POSTGRES);
}

#[tokio::test]
async fn test_set_transaction_serializable_skips_cache() {
    let value = read_in_transaction(
        true,
        &[
            "BEGIN READ ONLY ISOLATION LEVEL READ COMMITTED",
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
        ],
    )
    .await;
    assert_eq!(value, FROM_POSTGRES);
}

#[tokio::test]
async fn test_session_default_serializable_skips_cache() {
    let value = read_in_transaction(
        true,
        &[
            "SET default_transaction_isolation TO 'serializable'",
            "BEGIN READ ONLY",
        ],
    )
    .await;
    assert_eq!(value, FROM_POSTGRES);
}

#[tokio::test]
async fn test_session_characteristics_serializable_skips_cache() {
    let value = read_in_transaction(
        true,
        &[
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "BEGIN READ ONLY",
        ],
    )
    .await;
    assert_eq!(value, FROM_POSTGRES);
}

/// Hit inside a transaction, of a result stored outside one: the client
/// must be told it's in a transaction.
#[tokio::test]
async fn test_hit_in_transaction_reports_transaction() {
    enable(false);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    prime_cache(&mut client, &mut table).await;
    table.update_directly(1000).await;

    simple(
        &mut client,
        "BEGIN READ ONLY ISOLATION LEVEL READ COMMITTED",
    )
    .await;
    let reply = simple(&mut client, &table.select()).await;
    simple(&mut client, "COMMIT").await;
    table.drop().await;

    assert!(reply.iter().any(|message| message.code() == 'D'), "no rows");
    assert_eq!(status(&reply), 'T', "told it's idle in a transaction");
}

/// Result stored inside a transaction, hit outside: the client must be told
/// it's idle.
#[tokio::test]
async fn test_stored_in_transaction_hit_outside_reports_idle() {
    enable(false);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    simple(
        &mut client,
        "BEGIN READ ONLY ISOLATION LEVEL READ COMMITTED",
    )
    .await;
    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );
    simple(&mut client, "COMMIT").await;
    table.update_directly(1000).await;
    let reply = simple(&mut client, &table.select()).await;
    table.drop().await;

    let value = reply
        .iter()
        .find(|message| message.code() == 'D')
        .and_then(|message| crate::net::DataRow::try_from(message.clone()).ok())
        .and_then(|row| row.get_text(0));
    assert_eq!(value.as_deref(), Some("1"), "not stored in the transaction");
    assert_eq!(status(&reply), 'I', "told it's in a transaction");
}

/// After an error, Postgres refuses every statement until ROLLBACK: the
/// cache must not answer for it.
#[tokio::test]
async fn test_failed_transaction_skips_cache() {
    enable(true);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    prime_cache(&mut client, &mut table).await;

    simple(
        &mut client,
        "BEGIN READ ONLY ISOLATION LEVEL READ COMMITTED",
    )
    .await;
    reply(&mut client, "SELECT 1/0").await;
    let reply = reply(&mut client, &table.select()).await;
    simple(&mut client, "ROLLBACK").await;
    table.drop().await;

    assert!(
        reply.iter().any(|message| message.code() == 'E'),
        "cached rows served in a failed transaction"
    );
    assert_eq!(status(&reply), 'E');
}
