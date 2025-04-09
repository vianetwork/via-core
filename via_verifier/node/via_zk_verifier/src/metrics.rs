use std::time::Duration;

use vise::{Buckets, Gauge, Histogram, Metrics, Unit};

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_zk")]
pub struct ViaZKVerifierMetrics {
    #[metrics(buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    pub verification_time: Histogram<Duration>,

    /// Last valid l1 batch number.
    pub last_valid_l1_batch: Gauge<usize>,

    /// Last invalid l1 batch number.
    pub last_invalid_l1_batch: Gauge<usize>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaZKVerifierMetrics> = vise::Global::new();
