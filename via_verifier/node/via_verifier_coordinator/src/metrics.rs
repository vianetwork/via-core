use std::time::Duration;

use vise::{Buckets, Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, Histogram, Metrics};

use crate::types::SessionType;

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

    /// Invalid session message for a batch number.
    pub session_invalid_message: Gauge<usize>,

    /// The last valid session for a batch number.
    pub session_last_valid_session: Gauge<usize>,

    /// The BTC balance of the account used in musig2 sessions.
    pub musig2_session_account_balance: Gauge<usize>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierCoordinatorMetrics> = vise::Global::new();
