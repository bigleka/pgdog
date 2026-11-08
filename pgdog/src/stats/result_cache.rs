use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static STORES: AtomicU64 = AtomicU64::new(0);
static BYTES_SERVED: AtomicU64 = AtomicU64::new(0);
static BYTES_STORED: AtomicU64 = AtomicU64::new(0);
static REDIS_ERRORS: AtomicU64 = AtomicU64::new(0);
static COALESCED: AtomicU64 = AtomicU64::new(0);
static EARLY_REFRESHES: AtomicU64 = AtomicU64::new(0);
static TTL_EXTENSIONS: AtomicU64 = AtomicU64::new(0);
static REMOTE_WAITS: AtomicU64 = AtomicU64::new(0);
static READ_PRIMARY: AtomicU64 = AtomicU64::new(0);
static REMOTE_COALESCED: AtomicU64 = AtomicU64::new(0);

pub struct ResultCacheMetric {
    name: String,
    help: String,
    value: u64,
    gauge: bool,
}

pub struct ResultCache;

impl ResultCache {
    pub fn hit(bytes: usize) {
        HITS.fetch_add(1, Ordering::Relaxed);
        BYTES_SERVED.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn miss() {
        MISSES.fetch_add(1, Ordering::Relaxed);
    }

    pub fn store(bytes: usize) {
        STORES.fetch_add(1, Ordering::Relaxed);
        BYTES_STORED.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn redis_error() {
        REDIS_ERRORS.fetch_add(1, Ordering::Relaxed);
    }

    /// A miss answered with another client's in-flight result.
    pub fn coalesced(bytes: usize) {
        COALESCED.fetch_add(1, Ordering::Relaxed);
        BYTES_SERVED.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// A hit picked by XFetch to recompute its entry before expiry.
    pub fn early_refresh() {
        EARLY_REFRESHES.fetch_add(1, Ordering::Relaxed);
    }

    /// A hit pushed its entry's expiry forward (adaptive TTL).
    pub fn ttl_extension() {
        TTL_EXTENSIONS.fetch_add(1, Ordering::Relaxed);
    }

    /// A miss found another PgDog running the same query and waited for it.
    pub fn remote_wait() {
        REMOTE_WAITS.fetch_add(1, Ordering::Relaxed);
    }

    /// A miss answered with the result another PgDog computed.
    pub fn remote_coalesced(bytes: usize) {
        REMOTE_COALESCED.fetch_add(1, Ordering::Relaxed);
        BYTES_SERVED.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// A read sent to the primary because a table it reads was just written.
    pub fn read_primary() {
        READ_PRIMARY.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn metrics() -> Vec<Metric> {
        vec![
            Metric::new(ResultCacheMetric {
                name: "result_cache_hits".into(),
                help: "Result cache hits (cached SELECT results served)".into(),
                value: HITS.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_misses".into(),
                help: "Result cache misses (cacheable SELECT not found)".into(),
                value: MISSES.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_stores".into(),
                help: "Result cache stores (cache entries written)".into(),
                value: STORES.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_bytes_served".into(),
                help: "Bytes served from result cache".into(),
                value: BYTES_SERVED.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_bytes_stored".into(),
                help: "Bytes stored into result cache".into(),
                value: BYTES_STORED.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_coalesced".into(),
                help: "Result cache misses served from another client's in-flight query".into(),
                value: COALESCED.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_remote_waits".into(),
                help: "Result cache misses that waited for another PgDog running the same query"
                    .into(),
                value: REMOTE_WAITS.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_remote_coalesced".into(),
                help: "Result cache misses served from the result of another PgDog".into(),
                value: REMOTE_COALESCED.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_early_refreshes".into(),
                help: "Result cache entries recomputed before expiry (XFetch)".into(),
                value: EARLY_REFRESHES.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_ttl_extensions".into(),
                help: "Result cache entries kept alive longer by a hit (adaptive TTL)".into(),
                value: TTL_EXTENSIONS.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_read_after_write_primary_reads".into(),
                help: "Reads sent to the primary because a table they read was just written".into(),
                value: READ_PRIMARY.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_redis_errors".into(),
                help: "Redis errors encountered by result cache".into(),
                value: REDIS_ERRORS.load(Ordering::Relaxed),
                gauge: false,
            }),
        ]
    }
}

impl OpenMetric for ResultCacheMetric {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn metric_type(&self) -> String {
        if self.gauge {
            "gauge".into()
        } else {
            "counter".into()
        }
    }

    fn help(&self) -> Option<String> {
        Some(self.help.clone())
    }

    fn measurements(&self) -> Vec<Measurement> {
        vec![Measurement {
            labels: vec![],
            measurement: MeasurementType::Integer(self.value as i64),
        }]
    }
}
