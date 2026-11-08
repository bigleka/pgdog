//! Adaptive TTL, end to end. Require Redis on 127.0.0.1:6379.

use std::{ops::Deref, time::Duration};

use pgdog_config::QueryParserLevel;
use tokio::time::sleep;

use crate::config::{config, set};

use super::{
    prelude::*,
    result_cache::{enable_result_cache, select_value, TestTable},
};

const TTL_SECS: u64 = 2;
const MAX_AGE_SECS: u64 = 4;

fn configure(adaptive_ttl_max_seconds: Option<u64>) {
    enable_result_cache(QueryParserLevel::Auto);
    let mut config = config().deref().clone();
    if let Some(result_cache) = config.config.result_cache.as_mut() {
        result_cache.expire_seconds = Some(TTL_SECS);
        result_cache.adaptive_ttl_max_seconds = adaptive_ttl_max_seconds;
        // Keep XFetch from refreshing entries while we watch them expire.
        result_cache.xfetch_beta = Some(0.0);
    }
    set(config).unwrap();
}

/// Hits keep an entry alive past `expire_seconds`, but never past `adaptive_ttl_max_seconds`.
#[tokio::test]
async fn test_hits_extend_entry_up_to_max_age() {
    configure(Some(MAX_AGE_SECS));
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    // t = 0: cached for 2s. The row then changes outside PgDog, so a hit returns "1".
    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );
    table.update_directly(2).await;

    // t = 1.2: hit, expiry moves to 3.2.
    sleep(Duration::from_millis(1200)).await;
    let first = select_value(&mut client, &table.select()).await;
    // t = 2.4: past the original expiry, still cached. Expiry moves to the 4s cap.
    sleep(Duration::from_millis(1200)).await;
    let second = select_value(&mut client, &table.select()).await;
    // t = 3.6: hit again; without the cap this would keep it until 5.6.
    sleep(Duration::from_millis(1200)).await;
    let third = select_value(&mut client, &table.select()).await;
    // t = 4.6: past the cap.
    sleep(Duration::from_millis(1000)).await;
    let after_cap = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(first.as_deref(), Some("1"));
    assert_eq!(
        second.as_deref(),
        Some("1"),
        "hit didn't extend the entry's TTL"
    );
    assert_eq!(third.as_deref(), Some("1"));
    assert_eq!(
        after_cap.as_deref(),
        Some("2"),
        "entry outlived adaptive_ttl_max_seconds"
    );
}

/// Without adaptive TTL, hits don't change when an entry expires.
#[tokio::test]
async fn test_entry_expires_on_time_without_adaptive_ttl() {
    configure(None);
    let mut table = TestTable::new().await;
    let mut client = TestClient::new(Parameters::default()).await;

    assert_eq!(
        select_value(&mut client, &table.select()).await.as_deref(),
        Some("1")
    );
    table.update_directly(2).await;

    sleep(Duration::from_millis(1200)).await;
    let before_expiry = select_value(&mut client, &table.select()).await;
    sleep(Duration::from_millis(1200)).await;
    let after_expiry = select_value(&mut client, &table.select()).await;
    table.drop().await;

    assert_eq!(before_expiry.as_deref(), Some("1"));
    assert_eq!(after_expiry.as_deref(), Some("2"));
}
