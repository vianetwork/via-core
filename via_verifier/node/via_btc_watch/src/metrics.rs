use vise::{Counter, EncodeLabelSet, EncodeLabelValue, Family, Metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "error_type", rename_all = "snake_case")]
pub enum ErrorType {
    InternalError,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_btc_watch")]
pub struct ViaVerifierBtcWatcherMetrics {
    /// Number of times Bitcoin was polled.
    pub btc_poll: Counter,

    /// Number of errors encountered, labeled by error type.
    pub errors: Family<ErrorType, Counter>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierBtcWatcherMetrics> = vise::Global::new();
