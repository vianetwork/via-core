use vise::{Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, Metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "stage", rename_all = "snake_case")]
pub enum ReorgType {
    /// When the reorg is hard it means a revert is required.
    Hard,
    /// When the reorg is soft no revert is required, the node just restart indexing from the last valid block.
    Soft,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_reorg_detector")]
pub struct ViaReorgMetrics {
    /// Detect when a reorg is found and set the first affected block.
    pub reorg_type: Family<ReorgType, Gauge<usize>>,
    /// Counter to store the layer errors.
    pub errors: Counter,
}

#[vise::register]
pub static METRICS: vise::Global<ViaReorgMetrics> = vise::Global::new();
