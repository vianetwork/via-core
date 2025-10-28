use vise::{Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, Metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue)]
#[metrics(rename_all = "snake_case")]
pub enum ErrorKind {
    PartialSignature,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, EncodeLabelSet)]
pub struct VerifierErrorLabel {
    pub pubkey: String,
    pub kind: ErrorKind,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_coordinator")]
pub struct ViaVerifierCoordinatorMetrics {
    /// The time duration to process a session in seconds.
    pub session_time: Gauge<usize>,

    /// Verifier errors
    pub errors: Counter,

    /// Verifier signatures error
    pub verifier_errors: Family<VerifierErrorLabel, Counter>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierCoordinatorMetrics> = vise::Global::new();
