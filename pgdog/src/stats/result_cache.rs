use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static STORES: AtomicU64 = AtomicU64::new(0);
static BYTES_SERVED: AtomicU64 = AtomicU64::new(0);
static BYTES_STORED: AtomicU64 = AtomicU64::new(0);
static REDIS_ERRORS: AtomicU64 = AtomicU64::new(0);
static SINGLEFLIGHT_JOINED: AtomicU64 = AtomicU64::new(0);
static SINGLEFLIGHT_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
static TTL_EXTENDED: AtomicU64 = AtomicU64::new(0);
static XFETCH_TRIGGERS: AtomicU64 = AtomicU64::new(0);
static DISTRIBUTED_SINGLEFLIGHT_JOINED: AtomicU64 = AtomicU64::new(0);
static READ_AFTER_WRITE_BYPASSES: AtomicU64 = AtomicU64::new(0);

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

    pub fn singleflight_joined(bytes: usize) {
        SINGLEFLIGHT_JOINED.fetch_add(1, Ordering::Relaxed);
        HITS.fetch_add(1, Ordering::Relaxed);
        BYTES_SERVED.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn singleflight_timeout() {
        SINGLEFLIGHT_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn ttl_extended() {
        TTL_EXTENDED.fetch_add(1, Ordering::Relaxed);
    }

    pub fn xfetch_trigger() {
        XFETCH_TRIGGERS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn distributed_singleflight_joined(bytes: usize) {
        DISTRIBUTED_SINGLEFLIGHT_JOINED.fetch_add(1, Ordering::Relaxed);
        HITS.fetch_add(1, Ordering::Relaxed);
        BYTES_SERVED.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn read_after_write_bypass() {
        READ_AFTER_WRITE_BYPASSES.fetch_add(1, Ordering::Relaxed);
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
                name: "result_cache_redis_errors".into(),
                help: "Redis errors encountered by result cache".into(),
                value: REDIS_ERRORS.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_singleflight_joined".into(),
                help: "Singleflight joined count (follower requests coalesced)".into(),
                value: SINGLEFLIGHT_JOINED.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_singleflight_timeouts".into(),
                help: "Singleflight waiter timeouts".into(),
                value: SINGLEFLIGHT_TIMEOUTS.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_ttl_extended".into(),
                help: "Adaptive TTL extensions performed".into(),
                value: TTL_EXTENDED.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_xfetch_triggers".into(),
                help: "XFetch probabilistic early background refreshes triggered".into(),
                value: XFETCH_TRIGGERS.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_distributed_singleflight_joined".into(),
                help: "Distributed cross-node singleflight requests coalesced".into(),
                value: DISTRIBUTED_SINGLEFLIGHT_JOINED.load(Ordering::Relaxed),
                gauge: false,
            }),
            Metric::new(ResultCacheMetric {
                name: "result_cache_read_after_write_bypasses".into(),
                help: "Cache lookups bypassed due to read-after-write session consistency".into(),
                value: READ_AFTER_WRITE_BYPASSES.load(Ordering::Relaxed),
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

