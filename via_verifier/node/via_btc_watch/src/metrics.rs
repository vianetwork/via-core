use vise::{Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, LabeledFamily, Metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "stage", rename_all = "snake_case")]
pub enum InscriptionStage {
    IndexedL1Batch,
    Upgrade,
    Finalized,
    Reorg,
    Vote,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_btc_watch")]
pub struct ViaVerifierBtcWatcherMetrics {
    #[metrics(labels = ["role", "address"])]
    pub system_wallets: LabeledFamily<(String, String), Counter, 2>,

    /// Number of inscriptions processed, labeled by type.
    pub inscriptions_processed: Family<InscriptionStage, Gauge<usize>>,

    /// Deposit processed.
    pub deposit: Counter,

    /// Withdrawal confirmed.
    pub withdrawal_confirmed: Counter,

    /// Errors
    pub errors: Counter,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierBtcWatcherMetrics> = vise::Global::new();
