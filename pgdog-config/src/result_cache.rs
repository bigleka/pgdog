use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResultCache {
    /// Enable Redis/RESP result cache.
    #[serde(default)]
    pub enabled: bool,

    /// Redis connection URL, e.g. redis://127.0.0.1:6379
    #[serde(default)]
    pub redis_url: String,

    /// How PgDog connects to Redis.
    ///
    /// _Default:_ `standalone`
    #[serde(default)]
    pub redis_mode: RedisMode,

    /// Redis nodes for `redis_mode = "cluster"` (one or more cluster nodes to
    /// discover the cluster from) or `"sentinel"` (the Sentinels). Falls back
    /// to `redis_url` when empty.
    #[serde(default)]
    pub redis_urls: Vec<String>,

    /// Name of the primary Sentinel monitors, for `redis_mode = "sentinel"`.
    pub redis_sentinel_master: Option<String>,

    /// How long connecting to Redis may take before it counts as down
    /// (milliseconds). See "Redis timeouts" in docs/RESULT_CACHE.md before
    /// changing it.
    ///
    /// _Default:_ `500`
    pub redis_connect_timeout_ms: Option<u64>,

    /// How long a Redis command may take before Redis counts as down
    /// (milliseconds). Every cacheable query waits on Redis at least once, so
    /// this is also the most a slow Redis adds to a query.
    ///
    /// _Default:_ `500`
    pub redis_response_timeout_ms: Option<u64>,

    /// How many times connecting to Redis is retried before it counts as down.
    ///
    /// _Default:_ `1`
    pub redis_connect_retries: Option<u32>,

    /// Once Redis counts as down, how long the cache is skipped, queries
    /// going straight to Postgres, before connecting again (milliseconds).
    /// `0` tries again on the next query.
    ///
    /// _Default:_ `5000`
    pub redis_unavailable_backoff_ms: Option<u64>,

    /// Optional expiration time for cached entries (seconds).
    pub expire_seconds: Option<u64>,

    /// Maximum entry size to cache (bytes). If not set, PgDog uses a safe default.
    pub max_entry_bytes: Option<usize>,

    /// Key prefix in Redis.
    pub key_prefix: Option<String>,

    /// Optional secret key for encryption.
    pub encryption_key: Option<String>,

    /// How eagerly hits refresh entries before they expire (XFetch).
    ///
    /// A hit refreshes its entry when `-compute_time * xfetch_beta * ln(rand)`
    /// reaches the entry's remaining TTL, so hot and slow queries are
    /// recomputed by one client ahead of expiry instead of by every client
    /// after it. Values above `1.0` refresh earlier, below later; `0` disables
    /// early refresh.
    ///
    /// _Default:_ `1.0`
    pub xfetch_beta: Option<f64>,

    /// Enable adaptive TTL: each hit pushes the entry's expiry to
    /// `expire_seconds` from now, so frequently read entries stay cached,
    /// but never longer than this many seconds after the query ran.
    ///
    /// **Note:** changes made outside PgDog can stay invisible for up to
    /// this long, instead of `expire_seconds`. Writes through PgDog still
    /// invalidate entries immediately.
    ///
    /// _Default:_ disabled
    pub adaptive_ttl_max_seconds: Option<u64>,

    /// Coalesce misses between PgDogs sharing Redis: the first one to miss
    /// on a query takes a lock in Redis and runs it, the others wait for its
    /// result instead of running the same query.
    ///
    /// The lock is renewed while the query runs and expires this many
    /// milliseconds after the PgDog holding it stops renewing it, e.g. when
    /// it dies. That's how long the others wait before running the query
    /// themselves.
    ///
    /// `0` disables the lock: each PgDog only coalesces its own clients.
    ///
    /// _Default:_ `1000`
    pub singleflight_lock_ms: Option<u64>,

    /// Optional allow-list of schemas whose tables can be cached.
    ///
    /// Each entry is treated as a regular expression.
    #[serde(default)]
    pub cache_safe_schema_list: Vec<String>,

    /// Optional deny-list of schemas whose tables should never be cached.
    ///
    /// Takes precedence over `cache_safe_schema_list`.
    /// Each entry is treated as a regular expression.
    #[serde(default)]
    pub cache_unsafe_schema_list: Vec<String>,

    /// Optional allow-list of tables whose results can be cached.
    ///
    /// Each entry is treated as a regular expression and matched against
    /// either `table` or `schema.table` when schema is present.
    #[serde(default)]
    pub cache_safe_table_list: Vec<String>,

    /// Optional deny-list of tables whose results should never be cached.
    ///
    /// Takes precedence over `cache_safe_table_list`.
    /// Each entry is treated as a regular expression and matched against
    /// either `table` or `schema.table` when schema is present.
    #[serde(default)]
    pub cache_unsafe_table_list: Vec<String>,
}

/// How the result cache connects to Redis.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RedisMode {
    /// One Redis server at `redis_url`, or a proxy such as twemproxy or Envoy in front of several.
    #[default]
    Standalone,
    /// Redis Cluster, discovered from `redis_urls`.
    Cluster,
    /// Primary `redis_sentinel_master`, found through the Sentinels in `redis_urls`.
    Sentinel,
}
