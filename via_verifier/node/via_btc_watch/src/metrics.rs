use vise::{Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, LabeledFamily, Metrics, Unit};

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

    /// Unix timestamp of the last completed loop iteration. Liveness only; does not assert that the iteration's work succeeded.
    #[metrics(unit = Unit::Seconds)]
    pub last_iteration_timestamp: Gauge<u64>,
}

impl ViaVerifierBtcWatcherMetrics {
    pub(crate) fn initialize(&self) {
        self.inscriptions_processed[&InscriptionStage::IndexedL1Batch].inc_by(0);
        self.inscriptions_processed[&InscriptionStage::Upgrade].inc_by(0);
        self.inscriptions_processed[&InscriptionStage::Finalized].inc_by(0);
        self.inscriptions_processed[&InscriptionStage::Reorg].inc_by(0);
        self.inscriptions_processed[&InscriptionStage::Vote].inc_by(0);
        self.deposit.inc_by(0);
        self.withdrawal_confirmed.inc_by(0);
        self.errors.inc_by(0);
        self.last_iteration_timestamp.inc_by(0);
    }
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierBtcWatcherMetrics> = vise::Global::new();

#[cfg(test)]
#[test]
fn materializes_metrics_before_work() {
    let registry = vise::MetricsCollection::lazy().filter(|group| group.module_path == module_path!()).collect();
    METRICS.initialize();
    let mut output = String::new();
    registry.encode(&mut output, vise::Format::OpenMetricsForPrometheus).unwrap();
    let samples: Vec<_> = output.lines().filter(|line| !line.starts_with('#')).collect();
    assert!(samples.len() >= 9, "{samples:#?}");
    assert!(["indexed_l1_batch", "upgrade", "finalized", "reorg", "vote"]
        .iter()
        .all(|stage| output.contains(&format!("stage=\"{stage}\""))));
    assert!(samples.contains(&"via_verifier_btc_watch_last_iteration_timestamp_seconds 0"));
}
