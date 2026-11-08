pub mod connection;
pub mod entry;
pub mod lock;
pub mod redis;
pub mod singleflight;
pub mod ttl;
pub mod xfetch;

use std::time::Duration;

use crate::frontend::router::parser::OwnedTable;
use crate::frontend::ClientRequest;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResultCacheKey {
    pub redis_key: String,
    pub ttl: Option<Duration>,
    pub max_entry_bytes: usize,
    /// A table the query reads was written within the read-after-write
    /// window: run it on the primary.
    pub read_primary: bool,
}

/// How long after a write reads of the table go to the primary, if enabled.
pub fn read_after_write_window() -> Option<Duration> {
    let window = crate::config::config()
        .config
        .result_cache
        .as_ref()
        .filter(|result_cache| result_cache.enabled)
        .map(|result_cache| result_cache.read_after_write_window_ms.unwrap_or(1000))?;
    Some(Duration::from_millis(window)).filter(|window| !window.is_zero())
}

#[derive(Debug, Clone)]
pub struct CacheableRequest {
    pub query: String,
    pub route_sig: &'static str,
    pub tables: Vec<OwnedTable>,
}

impl CacheableRequest {
    pub fn from_client_request(client_request: &ClientRequest) -> Option<Self> {
        // MVP: only simple query protocol, exactly one Query message.
        if client_request.messages.len() != 1 {
            return None;
        }

        let buffered = client_request.query().ok()??;
        let query = match buffered {
            crate::frontend::BufferedQuery::Query(q) => q.query().to_string(),
            _ => return None,
        };

        let route = client_request.route();
        if !route.is_read() {
            return None;
        }

        let route_sig = if route.is_cross_shard() {
            "cross"
        } else {
            "direct"
        };
        let tables = client_request
            .ast
            .as_ref()
            .map(|ast| ast.tables().into_iter().map(|t| t.to_owned()).collect())
            .unwrap_or_default();

        Some(Self {
            query,
            route_sig,
            tables,
        })
    }
}

#[cfg(test)]
mod test;
