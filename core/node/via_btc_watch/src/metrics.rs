use vise::{EncodeLabelSet, EncodeLabelValue, Family, Gauge, Metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "stage", rename_all = "snake_case")]
pub enum InscriptionStage {
    Vote,
    Deposit,
    Upgrade,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_server_btc_watch")]
pub struct ViaBtcWatcherMetrics {
    /// Number of inscriptions processed, labeled by type.
    pub inscriptions_processed: Family<InscriptionStage, Gauge<usize>>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaBtcWatcherMetrics> = vise::Global::new();
