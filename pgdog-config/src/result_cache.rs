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

    /// Optional expiration time for cached entries (seconds).
    pub expire_seconds: Option<u64>,

    /// Maximum entry size to cache (bytes). If not set, PgDog uses a safe default.
    pub max_entry_bytes: Option<usize>,

    /// Key prefix in Redis.
    pub key_prefix: Option<String>,

    /// Optional secret key for encryption.
    pub encryption_key: Option<String>,

    /// Enable singleflight (cache stampede protection). Defaults to true if not specified.
    pub singleflight_enabled: Option<bool>,

    /// Timeout in milliseconds for singleflight waiters. Defaults to 5000ms.
    pub singleflight_timeout_ms: Option<u64>,

    /// Enable adaptive dynamic TTL for frequently requested keys. Defaults to true if not specified.
    pub adaptive_ttl_enabled: Option<bool>,

    /// Maximum expiration time for cached entries under adaptive TTL (seconds). Defaults to 300s.
    pub max_expire_seconds: Option<u64>,

    /// Enable XFetch probabilistic early recomputation. Defaults to true if not specified.
    pub xfetch_enabled: Option<bool>,

    /// Aggressiveness factor beta for XFetch probabilistic early recomputation. Defaults to 1.0.
    pub xfetch_beta: Option<f64>,

    /// Enable distributed singleflight across cluster nodes via Redis. Defaults to true if not specified.
    pub distributed_singleflight_enabled: Option<bool>,

    /// Timeout in milliseconds for distributed singleflight waiters. Defaults to 3000ms.
    pub distributed_singleflight_timeout_ms: Option<u64>,

    /// Enable read-after-write session consistency. Defaults to true if not specified.
    pub read_after_write_consistency_enabled: Option<bool>,

    /// Window in milliseconds for session consistency after a DML. Defaults to 2000ms.
    pub read_after_write_window_ms: Option<u64>,

    /// Enable granular row/key-level cache invalidation. Reserved for future use.
    pub granular_invalidation_enabled: Option<bool>,


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


