use std::time::Duration;

use vise::{Buckets, Counter, Gauge, Histogram, Metrics, Unit};

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_zk")]
pub struct ViaZKVerifierMetrics {
    #[metrics(buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    pub verification_time: Histogram<Duration>,

    /// Last valid l1 batch number.
    pub last_valid_l1_batch: Gauge<usize>,

    /// Last invalid l1 batch number.
    pub last_invalid_l1_batch: Gauge<usize>,

    /// Errors
    pub errors: Counter,

    /// Unix timestamp of the last completed loop iteration. Liveness only; does not assert that the iteration's work succeeded.
    #[metrics(unit = Unit::Seconds)]
    pub last_iteration_timestamp: Gauge<u64>,
}

impl ViaZKVerifierMetrics {
    pub(crate) fn initialize(&self) {
        self.last_valid_l1_batch.inc_by(0);
        self.last_invalid_l1_batch.inc_by(0);
        self.errors.inc_by(0);
        self.last_iteration_timestamp.inc_by(0);
    }
}

#[vise::register]
pub static METRICS: vise::Global<ViaZKVerifierMetrics> = vise::Global::new();

#[cfg(test)]
#[test]
fn materializes_metrics_before_work() {
    let registry = vise::MetricsCollection::lazy()
        .filter(|group| group.module_path == module_path!())
        .collect();
    METRICS.initialize();
    let mut output = String::new();
    registry
        .encode(&mut output, vise::Format::OpenMetricsForPrometheus)
        .unwrap();
    assert_eq!(output.matches("_l1_batch 0\n").count(), 2, "{output}");
    assert!(output.contains("\nvia_verifier_zk_errors 0\n"), "{output}");
    assert!(output.contains("\nvia_verifier_zk_last_iteration_timestamp_seconds 0\n"));
}
