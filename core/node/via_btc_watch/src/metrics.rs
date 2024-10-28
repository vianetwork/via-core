use vise::{Counter, EncodeLabelSet, EncodeLabelValue, Family, Metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "stage", rename_all = "snake_case")]
pub enum InscriptionStage {
    Deposit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "error_type", rename_all = "snake_case")]
pub enum ErrorType {
    InternalError,
    DatabaseError,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_server_btc_watch")]
pub struct ViaBtcWatcherMetrics {
    /// Number of times Bitcoin was polled.
    pub btc_poll: Counter,

    /// Number of inscriptions processed, labeled by type.
    pub inscriptions_processed: Family<InscriptionStage, Counter>,

    /// Number of errors encountered, labeled by error type.
    pub errors: Family<ErrorType, Counter>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaBtcWatcherMetrics> = vise::Global::new();
