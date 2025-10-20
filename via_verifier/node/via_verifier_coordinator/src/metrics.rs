use std::time::Duration;

use vise::{Buckets, Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, Histogram, Metrics};

use crate::types::SessionType;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "error_type", rename_all = "snake_case")]
pub enum MetricSessionType {
    Withdrawal,
}

impl From<SessionType> for MetricSessionType {
    fn from(value: SessionType) -> Self {
        match value {
            SessionType::Withdrawal => MetricSessionType::Withdrawal,
        }
    }
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_coordinator")]
pub struct ViaVerifierCoordinatorMetrics {
    /// Counter track new sessions.
    pub session_new: Family<MetricSessionType, Counter>,

    /// The time duration to process a session in seconds.
    #[metrics(buckets = Buckets::LATENCIES, unit = vise::Unit::Seconds)]
    pub session_time: Histogram<Duration>,

    /// Invalid session message.
    pub session_invalid_message: Counter,

    /// Valid session.
    pub session_valid_session: Counter,

    /// The BTC balance of the account used in musig2 sessions.
    pub musig2_session_account_balance: Gauge<usize>,

    /// Errors
    pub verifier_errors: Family<VerifierErrorLabel, Counter>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierCoordinatorMetrics> = vise::Global::new();
