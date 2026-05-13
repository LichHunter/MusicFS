use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Default)]
pub struct Metrics {
    pub fuse_ops: FuseOpsMetrics,
    pub fuse_latency: FuseLatencyMetrics,
    pub cache: CacheMetrics,
    pub origins: OriginsMetrics,
    pub origin_health: OriginHealthMetrics,
    start_time: Option<Instant>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            start_time: Some(Instant::now()),
            ..Default::default()
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0)
    }

    pub fn to_prometheus(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!(
            "# HELP musicfs_fuse_ops_total Total FUSE operations\n\
             # TYPE musicfs_fuse_ops_total counter\n\
             musicfs_fuse_ops_total{{op=\"lookup\"}} {}\n\
             musicfs_fuse_ops_total{{op=\"getattr\"}} {}\n\
             musicfs_fuse_ops_total{{op=\"read\"}} {}\n\
             musicfs_fuse_ops_total{{op=\"readdir\"}} {}\n\
             musicfs_fuse_ops_total{{op=\"open\"}} {}\n",
            self.fuse_ops.lookup.load(Ordering::Relaxed),
            self.fuse_ops.getattr.load(Ordering::Relaxed),
            self.fuse_ops.read.load(Ordering::Relaxed),
            self.fuse_ops.readdir.load(Ordering::Relaxed),
            self.fuse_ops.open.load(Ordering::Relaxed),
        ));

        for (op, histogram) in self.fuse_latency.histograms.read().iter() {
            let quantiles = histogram.quantiles();
            output.push_str(&format!(
                "# HELP musicfs_fuse_latency_seconds FUSE operation latency\n\
                 # TYPE musicfs_fuse_latency_seconds summary\n\
                 musicfs_fuse_latency_seconds{{op=\"{}\",quantile=\"0.5\"}} {:.6}\n\
                 musicfs_fuse_latency_seconds{{op=\"{}\",quantile=\"0.95\"}} {:.6}\n\
                 musicfs_fuse_latency_seconds{{op=\"{}\",quantile=\"0.99\"}} {:.6}\n\
                 musicfs_fuse_latency_seconds_sum{{op=\"{}\"}} {:.6}\n\
                 musicfs_fuse_latency_seconds_count{{op=\"{}\"}} {}\n",
                op, quantiles.p50,
                op, quantiles.p95,
                op, quantiles.p99,
                op, histogram.sum_secs(),
                op, histogram.count(),
            ));
        }

        output.push_str(&format!(
            "# HELP musicfs_cache_hits_total Cache hits\n\
             # TYPE musicfs_cache_hits_total counter\n\
             musicfs_cache_hits_total {}\n",
            self.cache.hits.load(Ordering::Relaxed),
        ));

        output.push_str(&format!(
            "# HELP musicfs_cache_misses_total Cache misses\n\
             # TYPE musicfs_cache_misses_total counter\n\
             musicfs_cache_misses_total {}\n",
            self.cache.misses.load(Ordering::Relaxed),
        ));

        output.push_str(&format!(
            "# HELP musicfs_cache_size_bytes Current cache size in bytes\n\
             # TYPE musicfs_cache_size_bytes gauge\n\
             musicfs_cache_size_bytes {}\n",
            self.cache.size_bytes.load(Ordering::Relaxed),
        ));

        output.push_str(&format!(
            "# HELP musicfs_cache_chunks_total Number of cached chunks\n\
             # TYPE musicfs_cache_chunks_total gauge\n\
             musicfs_cache_chunks_total {}\n",
            self.cache.chunk_count.load(Ordering::Relaxed),
        ));

        output.push_str(
            "# HELP musicfs_origin_health Origin health status (1=healthy, 0=unhealthy)\n\
             # TYPE musicfs_origin_health gauge\n",
        );
        for (origin_id, healthy) in self.origin_health.status.read().iter() {
            output.push_str(&format!(
                "musicfs_origin_health{{origin=\"{}\"}} {}\n",
                origin_id,
                if *healthy { 1 } else { 0 }
            ));
        }

        output.push_str(&format!(
            "# HELP musicfs_uptime_seconds Daemon uptime in seconds\n\
             # TYPE musicfs_uptime_seconds gauge\n\
             musicfs_uptime_seconds {}\n",
            self.uptime_secs(),
        ));

        output
    }

    pub fn hit_ratio(&self) -> f64 {
        let hits = self.cache.hits.load(Ordering::Relaxed) as f64;
        let misses = self.cache.misses.load(Ordering::Relaxed) as f64;
        let total = hits + misses;

        if total == 0.0 {
            0.0
        } else {
            hits / total
        }
    }
}

#[derive(Default)]
pub struct FuseOpsMetrics {
    pub lookup: AtomicU64,
    pub getattr: AtomicU64,
    pub read: AtomicU64,
    pub readdir: AtomicU64,
    pub open: AtomicU64,
}

impl FuseOpsMetrics {
    pub fn record_lookup(&self) {
        self.lookup.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_getattr(&self) {
        self.getattr.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_read(&self) {
        self.read.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_readdir(&self) {
        self.readdir.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_open(&self) {
        self.open.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct CacheMetrics {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub size_bytes: AtomicU64,
    pub chunk_count: AtomicU64,
}

impl CacheMetrics {
    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn update_size(&self, size: u64) {
        self.size_bytes.store(size, Ordering::Relaxed);
    }

    pub fn update_chunk_count(&self, count: u64) {
        self.chunk_count.store(count, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct OriginsMetrics {
    pub healthy_count: AtomicU64,
    pub total_count: AtomicU64,
}

impl OriginsMetrics {
    pub fn update(&self, healthy: u64, total: u64) {
        self.healthy_count.store(healthy, Ordering::Relaxed);
        self.total_count.store(total, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct FuseLatencyMetrics {
    pub histograms: RwLock<HashMap<String, LatencyHistogram>>,
}

impl FuseLatencyMetrics {
    pub fn record(&self, op: &str, latency_secs: f64) {
        let mut histograms = self.histograms.write();
        histograms
            .entry(op.to_string())
            .or_default()
            .record(latency_secs);
    }
}

#[derive(Default)]
pub struct LatencyHistogram {
    samples: Vec<f64>,
    sum: f64,
}

impl LatencyHistogram {
    pub fn record(&mut self, latency_secs: f64) {
        self.samples.push(latency_secs);
        self.sum += latency_secs;

        if self.samples.len() > 10000 {
            self.samples.drain(..5000);
        }
    }

    pub fn quantiles(&self) -> Quantiles {
        if self.samples.is_empty() {
            return Quantiles::default();
        }

        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let len = sorted.len();
        Quantiles {
            p50: sorted[len / 2],
            p95: sorted[(len as f64 * 0.95) as usize],
            p99: sorted[(len as f64 * 0.99) as usize],
        }
    }

    pub fn sum_secs(&self) -> f64 {
        self.sum
    }

    pub fn count(&self) -> u64 {
        self.samples.len() as u64
    }
}

#[derive(Default)]
pub struct Quantiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

#[derive(Default)]
pub struct OriginHealthMetrics {
    pub status: RwLock<HashMap<String, bool>>,
}

impl OriginHealthMetrics {
    pub fn set_health(&self, origin_id: &str, healthy: bool) {
        self.status
            .write()
            .insert(origin_id.to_string(), healthy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_new() {
        let metrics = Metrics::new();
        assert!(metrics.uptime_secs() < 5);
    }

    #[test]
    fn test_fuse_ops_recording() {
        let metrics = Metrics::new();
        metrics.fuse_ops.record_lookup();
        metrics.fuse_ops.record_lookup();
        metrics.fuse_ops.record_read();

        assert_eq!(metrics.fuse_ops.lookup.load(Ordering::Relaxed), 2);
        assert_eq!(metrics.fuse_ops.read.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_cache_hit_ratio() {
        let metrics = Metrics::new();
        metrics.cache.hits.store(8, Ordering::Relaxed);
        metrics.cache.misses.store(2, Ordering::Relaxed);

        assert!((metrics.hit_ratio() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_cache_hit_ratio_zero() {
        let metrics = Metrics::new();
        assert_eq!(metrics.hit_ratio(), 0.0);
    }

    #[test]
    fn test_prometheus_format() {
        let metrics = Metrics::new();
        metrics.fuse_ops.record_lookup();
        metrics.cache.record_hit();

        let output = metrics.to_prometheus();
        assert!(output.contains("musicfs_fuse_ops_total"));
        assert!(output.contains("musicfs_cache_hits_total"));
    }
}
