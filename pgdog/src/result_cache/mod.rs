pub mod connection;
pub mod entry;
pub mod lock;
pub mod redis;
pub mod singleflight;
pub mod ttl;
pub mod xfetch;

use std::{borrow::Cow, time::Duration};

use crate::frontend::router::parser::OwnedTable;
use crate::frontend::ClientRequest;
use crate::net::{ProtocolMessage, ToBytes};

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
    /// Everything besides the query that changes the reply: which messages
    /// were sent (the reply has one message per request message), parameter
    /// types and values, and parameter and result formats.
    pub variant: Vec<u8>,
    pub route_sig: &'static str,
    pub tables: Vec<OwnedTable>,
}

impl CacheableRequest {
    pub fn from_client_request(client_request: &ClientRequest) -> Option<Self> {
        let variant = variant(&client_request.messages)?;

        let query = match client_request.query().ok()?? {
            crate::frontend::BufferedQuery::Query(query) => query.query().to_string(),
            crate::frontend::BufferedQuery::Prepared(parse) => {
                // Parameter types change how values are read.
                let mut variant = variant;
                variant.extend_from_slice(&parse.data_types_ref());
                return Self::new(client_request, parse.query().to_string(), variant);
            }
        };
        Self::new(client_request, query, variant)
    }

    fn new(client_request: &ClientRequest, query: String, variant: Vec<u8>) -> Option<Self> {
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
            variant,
            route_sig,
            tables,
        })
    }
}

/// Cached reply as sent to a client in or out of a transaction.
///
/// A reply ends with the ReadyForQuery it was captured with, whose status
/// says whether that client was in a transaction. Only read-only
/// transactions use the cache, so it's `I` or `T`.
pub fn reply_for(payload: &[u8], in_transaction: bool) -> Cow<'_, [u8]> {
    let status = if in_transaction { b'T' } else { b'I' };
    match payload {
        [.., b'Z', 0, 0, 0, 5, current] if *current != status => {
            let mut reply = payload.to_vec();
            if let Some(last) = reply.last_mut() {
                *last = status;
            }
            Cow::Owned(reply)
        }
        _ => Cow::Borrowed(payload),
    }
}

/// What, besides the query, the reply depends on, if the request can be
/// cached: one simple query, or one statement over the extended protocol
/// (`[Parse] Bind [Describe] Execute Sync`) returning all its rows.
fn variant(messages: &[ProtocolMessage]) -> Option<Vec<u8>> {
    use ProtocolMessage::*;

    if let [Query(_)] = messages {
        return Some(b"Q".to_vec());
    }

    let (parse, rest) = match messages {
        [Parse(_), rest @ ..] => (true, rest),
        rest => (false, rest),
    };
    let (bind, describe, execute) = match rest {
        [Bind(bind), Describe(describe), Execute(execute), Sync(_)] => {
            (bind, Some(describe), execute)
        }
        [Bind(bind), Execute(execute), Sync(_)] => (bind, None, execute),
        _ => return None,
    };
    // A row limit returns part of the result.
    if execute.max_rows() != 0 {
        return None;
    }

    let mut variant = Vec::new();
    if parse {
        variant.push(b'P');
    }
    variant.push(b'B');
    if let Some(describe) = describe {
        variant.extend_from_slice(&[b'D', describe.kind() as u8]);
    }
    variant.push(b'E');
    // Formats and values, not statement names: they're the client's.
    let mut bind = bind.clone();
    bind.rename("");
    variant.extend_from_slice(&bind.to_bytes().ok()?);
    Some(variant)
}

#[cfg(test)]
mod test;
