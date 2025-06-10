use vise::{Gauge, Metrics};

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_btc_watch")]
pub struct ViaVerifierBtcWatcherMetrics {
    /// Last indexed l1 batch number.
    pub last_indexed_block_number: Gauge<usize>,

    /// Last indexed l1 batch number.
    pub current_block_number: Gauge<usize>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierBtcWatcherMetrics> = vise::Global::new();
