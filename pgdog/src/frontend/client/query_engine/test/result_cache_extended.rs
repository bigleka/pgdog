//! Result cache over the extended protocol (Parse/Bind/Execute). Require
//! Redis on 127.0.0.1:6379.

use pgdog_config::QueryParserLevel;

use crate::net::{DataRow, Message};

use super::{
    prelude::*,
    result_cache::{TestTable, enable_result_cache, simple},
};

fn param(value: &str) -> Parameter {
    Parameter {
        len: value.len() as i32,
        data: value.as_bytes().to_vec().into(),
    }
}

/// Send one request and read the reply.
async fn request(client: &mut TestClient, messages: Vec<ProtocolMessage>) -> Vec<Message> {
    for message in messages {
        client.send(message).await;
    }
    client.try_process().await.unwrap();
    client.read_until('Z').await.unwrap()
}

fn codes(reply: &[Message]) -> String {
    reply.iter().map(|message| message.code()).collect()
}

/// First column of the first row, as text.
fn value(reply: &[Message]) -> Option<String> {
    reply
        .iter()
        .find(|message| message.code() == 'D')
        .and_then(|message| DataRow::try_from(message.clone()).ok())
        .and_then(|row| row.get_text(0))
}

/// Anonymous statement with a parameter: what most drivers send.
fn select_plus(table: &TestTable, add: &str) -> Vec<ProtocolMessage> {
    vec![
        Parse::new_anonymous(&format!(
            "SELECT v + $1::int FROM {} WHERE id = 1",
            table.name()
        ))
        .into(),
        Bind::new_params("", &[param(add)]).into(),
        Execute::new().into(),
        Sync.into(),
    ]
}

#[tokio::test]
async fn test_extended_query_is_cached() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    let miss = request(&mut client, select_plus(&table, "10")).await;
    table.update_directly(2).await;
    let hit = request(&mut client, select_plus(&table, "10")).await;
    table.drop().await;

    assert_eq!(value(&miss).as_deref(), Some("11"));
    assert_eq!(value(&hit).as_deref(), Some("11"), "wasn't a cache hit");
    assert_eq!(codes(&hit), codes(&miss));
}

#[tokio::test]
async fn test_extended_parameters_are_part_of_the_key() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    request(&mut client, select_plus(&table, "10")).await;
    let other = request(&mut client, select_plus(&table, "20")).await;
    table.drop().await;

    assert_eq!(
        value(&other).as_deref(),
        Some("21"),
        "served the result for another parameter"
    );
}

/// A hit replies with exactly the messages a miss would have, for each shape
/// of request.
#[tokio::test]
async fn test_extended_hit_replies_like_a_miss() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let query = format!("SELECT v FROM {} WHERE id = $1", table.name());

    let shapes: Vec<(&str, Vec<ProtocolMessage>)> = vec![
        (
            "describe portal",
            vec![
                Parse::named("raw_portal", &query).into(),
                Bind::new_params("raw_portal", &[param("1")]).into(),
                Describe::new_portal("").into(),
                Execute::new().into(),
                Sync.into(),
            ],
        ),
        (
            "describe statement",
            vec![
                Parse::named("raw_statement", &query).into(),
                Bind::new_params("raw_statement", &[param("1")]).into(),
                Describe::new_statement("raw_statement").into(),
                Execute::new().into(),
                Sync.into(),
            ],
        ),
        (
            "bind only",
            vec![
                Bind::new_params("raw_statement", &[param("1")]).into(),
                Execute::new().into(),
                Sync.into(),
            ],
        ),
    ];

    for (shape, messages) in shapes {
        table.update_directly(1).await;
        let miss = request(&mut client, messages.clone()).await;
        table.update_directly(2).await;
        let hit = request(&mut client, messages).await;

        assert_eq!(value(&hit).as_deref(), Some("1"), "{shape}: not a hit");
        assert_eq!(codes(&hit), codes(&miss), "{shape}: reply differs");
        // Invalidate, so the next shape starts with a miss.
        simple(
            &mut client,
            &format!("UPDATE {} SET v = 1 WHERE id = 1", table.name()),
        )
        .await;
    }
    table.drop().await;
}

/// Same SQL over the simple protocol: different reply (RowDescription, no
/// ParseComplete), so it mustn't be served the other's entry.
#[tokio::test]
async fn test_simple_and_extended_dont_share_entries() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    simple(&mut client, &table.select()).await;
    let extended = request(
        &mut client,
        vec![
            Parse::new_anonymous(&table.select()).into(),
            Bind::new_params("", &[]).into(),
            Execute::new().into(),
            Sync.into(),
        ],
    )
    .await;
    table.drop().await;

    assert_eq!(codes(&extended), "12DCZ");
}

/// Writes over the extended protocol invalidate.
#[tokio::test]
async fn test_extended_write_invalidates() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    request(&mut client, select_plus(&table, "0")).await;
    request(
        &mut client,
        vec![
            Parse::new_anonymous(&format!(
                "UPDATE {} SET v = $1::int WHERE id = 1",
                table.name()
            ))
            .into(),
            Bind::new_params("", &[param("5")]).into(),
            Execute::new().into(),
            Sync.into(),
        ],
    )
    .await;
    let after = request(&mut client, select_plus(&table, "0")).await;
    table.drop().await;

    assert_eq!(value(&after).as_deref(), Some("5"), "stale after write");
}

/// Execute with a row limit returns part of the result: not cacheable.
#[tokio::test]
async fn test_partial_execute_is_not_cached() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let select = table.select();
    let limited = || -> Vec<ProtocolMessage> {
        vec![
            Parse::new_anonymous(&select).into(),
            Bind::new_params("", &[]).into(),
            Execute::new_portal_limit("", 1).into(),
            Sync.into(),
        ]
    };

    request(&mut client, limited()).await;
    table.update_directly(2).await;
    let again = request(&mut client, limited()).await;
    table.drop().await;

    assert_eq!(value(&again).as_deref(), Some("2"), "partial result cached");
}

/// Several statements in one request (pipelining) aren't cached.
#[tokio::test]
async fn test_pipelined_statements_are_not_cached() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;
    let select = table.select();
    let pipelined = || -> Vec<ProtocolMessage> {
        vec![
            Parse::new_anonymous(&select).into(),
            Bind::new_params("", &[]).into(),
            Execute::new().into(),
            Parse::new_anonymous("SELECT 1").into(),
            Bind::new_params("", &[]).into(),
            Execute::new().into(),
            Sync.into(),
        ]
    };

    request(&mut client, pipelined()).await;
    table.update_directly(2).await;
    let again = request(&mut client, pipelined()).await;
    table.drop().await;

    assert_eq!(
        value(&again).as_deref(),
        Some("2"),
        "pipelined result cached"
    );
}

/// A named statement whose Parse was answered from the cache, so the server
/// never saw it, still works once a later Bind goes to the server.
#[tokio::test]
async fn test_statement_parsed_from_cache_runs_later() {
    enable_result_cache(QueryParserLevel::Auto);
    let table = TestTable::new().await;
    let query = format!("SELECT v FROM {} WHERE id = $1", table.name());
    let parse_bind = |name: &str| -> Vec<ProtocolMessage> {
        vec![
            Parse::named(name, &query).into(),
            Bind::new_params(name, &[param("1")]).into(),
            Execute::new().into(),
            Sync.into(),
        ]
    };
    let mut table = table;
    let mut first = TestClient::new(Parameters::default()).await;
    request(&mut first, parse_bind("from_cache")).await;
    table.update_directly(3).await;

    let mut second = TestClient::new(Parameters::default()).await;
    let hit = request(&mut second, parse_bind("from_cache")).await;
    simple(
        &mut second,
        &format!("UPDATE {} SET v = 7 WHERE id = 1", table.name()),
    )
    .await;
    let after = request(
        &mut second,
        vec![
            Bind::new_params("from_cache", &[param("1")]).into(),
            Execute::new().into(),
            Sync.into(),
        ],
    )
    .await;
    table.drop().await;

    assert_eq!(codes(&hit), "12DCZ");
    assert_eq!(value(&hit).as_deref(), Some("1"), "wasn't a cache hit");
    assert_eq!(value(&after).as_deref(), Some("7"), "{:?}", codes(&after));
}

/// Same text and value, different parameter type: different result.
#[tokio::test]
async fn test_parameter_types_are_part_of_the_key() {
    enable_result_cache(QueryParserLevel::Auto);
    let mut client = TestClient::new(Parameters::default()).await;
    let typed = |oid: i32| -> Vec<ProtocolMessage> {
        vec![
            Parse::new_anonymous_typed("SELECT $1", &[oid]).into(),
            Bind::new_params("", &[param("1.50")]).into(),
            Execute::new().into(),
            Sync.into(),
        ]
    };

    // numeric, then float8.
    let numeric = request(&mut client, typed(1700)).await;
    let float = request(&mut client, typed(701)).await;

    assert_eq!(value(&numeric).as_deref(), Some("1.50"));
    assert_eq!(
        value(&float).as_deref(),
        Some("1.5"),
        "served the numeric result"
    );
}
