use vise::{Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, Metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "stage", rename_all = "snake_case")]
pub enum ReorgInfo {
    StartBlock,
    EndBlock,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_reorg_detector")]
pub struct ViaVerifierReorgDetectorMetrics {
    /// The blocks affected by reorg.
    pub reorg_data: Family<ReorgInfo, Gauge<usize>>,

    /// Counter to tack soft reorgs.
    pub soft_reorg: Counter,

    /// Counter to tack hard reorgs.
    pub hard_reorg: Counter,

    /// Errors
    pub errors: Counter,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierReorgDetectorMetrics> = vise::Global::new();
