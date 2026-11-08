//! Connections to the Redis topologies the result cache supports.

use std::{fmt, time::Duration};

use pgdog_config::RedisMode;
use redis::{
    aio::{ConnectionLike, ConnectionManager, ConnectionManagerConfig},
    cluster::ClusterClient,
    cluster_async::ClusterConnection,
    Cmd, ConnectionInfo, IntoConnectionInfo, Pipeline, RedisFuture, RedisResult, Value,
};

/// The cache must never hold a query up for long: past these, Redis is
/// treated as unavailable and the query goes straight to Postgres.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);
const CONNECT_RETRIES: usize = 1;

/// Where the cache's Redis lives. A change means reconnecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub mode: RedisMode,
    pub urls: Vec<String>,
}

impl Target {
    /// `urls` for cluster mode, falling back to `url` when empty.
    pub fn new(mode: RedisMode, url: &str, urls: &[String]) -> Self {
        let urls = match mode {
            RedisMode::Cluster if !urls.is_empty() => urls.to_vec(),
            _ => vec![url.to_owned()],
        };
        Self { mode, urls }
    }
}

/// Connection to the cache's Redis. Cheap to clone; reconnects on its own.
#[derive(Clone)]
pub enum Connection {
    /// One server, or a proxy (twemproxy, Envoy) in front of several.
    Standalone(ConnectionManager),
    Cluster(ClusterConnection),
}

impl Connection {
    pub async fn open(target: &Target) -> RedisResult<Self> {
        let nodes = target
            .urls
            .iter()
            .map(|url| connection_info(url))
            .collect::<RedisResult<Vec<_>>>()?;

        match target.mode {
            RedisMode::Standalone => {
                let node = nodes.into_iter().next().ok_or_else(no_nodes)?;
                let config = ConnectionManagerConfig::new()
                    .set_connection_timeout(Some(CONNECT_TIMEOUT))
                    .set_response_timeout(Some(RESPONSE_TIMEOUT))
                    .set_number_of_retries(CONNECT_RETRIES);
                let client = redis::Client::open(node)?;
                Ok(Self::Standalone(
                    client.get_connection_manager_with_config(config).await?,
                ))
            }
            RedisMode::Cluster => {
                let client = ClusterClient::builder(nodes)
                    .connection_timeout(CONNECT_TIMEOUT)
                    .response_timeout(RESPONSE_TIMEOUT)
                    .retries(CONNECT_RETRIES as u32)
                    .build()?;
                Ok(Self::Cluster(client.get_async_connection().await?))
            }
        }
    }
}

/// Connection settings for `url`, without the `CLIENT SETINFO` handshake:
/// proxies like twemproxy close the connection on any `CLIENT` command.
fn connection_info(url: &str) -> RedisResult<ConnectionInfo> {
    let info = url.into_connection_info()?;
    let redis = info.redis_settings().clone().set_skip_set_lib_name();
    Ok(info.set_redis_settings(redis))
}

fn no_nodes() -> redis::RedisError {
    (
        redis::ErrorKind::InvalidClientConfig,
        "no Redis URL configured",
    )
        .into()
}

impl ConnectionLike for Connection {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            Self::Standalone(conn) => conn.req_packed_command(cmd),
            Self::Cluster(conn) => conn.req_packed_command(cmd),
        }
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        pipeline: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        match self {
            Self::Standalone(conn) => conn.req_packed_commands(pipeline, offset, count),
            Self::Cluster(conn) => conn.req_packed_commands(pipeline, offset, count),
        }
    }

    fn get_db(&self) -> i64 {
        match self {
            Self::Standalone(conn) => conn.get_db(),
            Self::Cluster(conn) => conn.get_db(),
        }
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standalone(_) => f.write_str("Connection::Standalone"),
            Self::Cluster(_) => f.write_str("Connection::Cluster"),
        }
    }
}
