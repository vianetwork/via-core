// jscpd:ignore-start Stage labels are local to the distinct main-node and verifier metric contracts.
use vise::{Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, LabeledFamily, Metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "stage", rename_all = "snake_case")]
pub enum InscriptionStage {
    Upgrade,
}

// jscpd:ignore-end

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_server_btc_watch")]
pub struct ViaBtcWatcherMetrics {
    #[metrics(labels = ["role", "address"])]
    pub system_wallets: LabeledFamily<(String, String), Counter, 2>,

    /// Number of inscriptions processed, labeled by type.
    pub inscriptions_processed: Family<InscriptionStage, Gauge<usize>>,

    /// Deposit processed.
    pub deposit: Counter,

    /// Counter to store the layer errors.
    pub errors: Counter,
}

#[vise::register]
pub static METRICS: vise::Global<ViaBtcWatcherMetrics> = vise::Global::new();
