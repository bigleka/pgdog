//! XFetch early refresh, end to end. Require Redis on 127.0.0.1:6379.

use std::{
    ops::Deref,
    time::{Duration, Instant},
};

use futures::future::join;
use pgdog_config::QueryParserLevel;
use rand::{rng, Rng};

use crate::{
    backend::server::test::test_server,
    config::{config, set},
};

use super::{
    prelude::*,
    result_cache::{enable_result_cache, select_value, TestTable},
};

/// Large enough that any hit refreshes the entry.
const ALWAYS: f64 = 1e9;

pub(super) fn set_xfetch_beta(beta: f64) {
    let mut config = config().deref().clone();
    if let Some(result_cache) = config.config.result_cache.as_mut() {
        result_cache.xfetch_beta = Some(beta);
    }
    set(config).unwrap();
}

#[tokio::test]
async fn test_hit_refreshes_entry_early() {
    enable_result_cache(QueryParserLevel::Auto);
    set_xfetch_beta(ALWAYS);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );
    table.update_directly(2).await;

    // The hit refreshes the entry, so this client sees the new row...
    let refreshed = select_value(&mut client, &table.select()).await;

    // ...and so does everyone after it, from the cache.
    set_xfetch_beta(0.0);
    table.update_directly(3).await;
    let cached = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(
        refreshed.as_deref(),
        Some("2"),
        "hit didn't refresh the entry"
    );
    assert_eq!(
        cached.as_deref(),
        Some("2"),
        "refreshed result wasn't stored"
    );
}

#[tokio::test]
async fn test_zero_beta_disables_early_refresh() {
    enable_result_cache(QueryParserLevel::Auto);
    set_xfetch_beta(0.0);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );
    table.update_directly(2).await;
    let value = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(value.as_deref(), Some("1"));
}

/// While one client refreshes a slow entry, the others are served the
/// current entry right away instead of waiting or querying.
#[tokio::test]
async fn test_hits_during_refresh_are_served_from_cache() {
    let suffix = rng().random::<u32>();
    let calls = format!("result_cache_calls_{suffix}");
    let function = format!("result_cache_counted_{suffix}");

    let mut direct = test_server().await;
    direct
        .execute(format!("CREATE TABLE {calls} (id SERIAL)"))
        .await
        .unwrap();
    // Returns how many times it ran, slowly.
    direct
        .execute(format!(
            "CREATE FUNCTION {function}() RETURNS BIGINT LANGUAGE plpgsql AS $$
             BEGIN INSERT INTO {calls} DEFAULT VALUES; PERFORM pg_sleep(0.5);
             RETURN (SELECT count(*) FROM {calls}); END $$"
        ))
        .await
        .unwrap();

    enable_result_cache(QueryParserLevel::On);
    set_xfetch_beta(ALWAYS);
    let mut refresher = TestClient::new(Parameters::default()).await;
    let mut reader = TestClient::new(Parameters::default()).await;
    let select = format!("SELECT {function}()");

    assert_eq!(
        select_value(&mut refresher, &select).await.as_deref(),
        Some("1")
    );

    let (refreshed, (read, read_time)) = join(select_value(&mut refresher, &select), async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let start = Instant::now();
        let value = select_value(&mut reader, &select).await;
        (value, start.elapsed())
    })
    .await;

    direct
        .execute(format!("DROP FUNCTION {function}(); DROP TABLE {calls}"))
        .await
        .unwrap();

    assert_eq!(refreshed.as_deref(), Some("2"), "first hit should refresh");
    assert_eq!(
        read.as_deref(),
        Some("1"),
        "reader should get the current entry"
    );
    assert!(
        read_time < Duration::from_millis(250),
        "reader waited {read_time:?} for the refresh"
    );
}
