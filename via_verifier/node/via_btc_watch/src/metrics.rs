use vise::{EncodeLabelSet, EncodeLabelValue, Family, Gauge, Metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "stage", rename_all = "snake_case")]
pub enum InscriptionStage {
    IndexedL1Batch,
    Deposit,
    Upgrade,
    Vote,
    Withdrawal,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_btc_watch")]
pub struct ViaVerifierBtcWatcherMetrics {
    /// Number of inscriptions processed, labeled by type.
    pub inscriptions_processed: Family<InscriptionStage, Gauge<usize>>,

    /// Last indexed l1 batch number.
    pub last_finalized_l1_batch: Gauge<usize>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierBtcWatcherMetrics> = vise::Global::new();
