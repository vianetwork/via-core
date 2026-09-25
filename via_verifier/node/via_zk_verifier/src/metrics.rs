use std::time::Duration;

use vise::{Buckets, Counter, EncodeLabelValue, Gauge, Histogram, LabeledFamily, Metrics, Unit};

/// Distinct reasons tell evidence that is still to arrive from a batch stuck until an operator acts.
/// Bitcoin Core's `VerifyDBResult` likewise separates skipped checks from corruption:
/// https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/validation.h#L388-L394
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue)]
#[metrics(rename_all = "snake_case")]
pub enum NoVerdictReason {
    PackageUnavailable,
    MalformedPackage,
    ProofUnavailable,
    ProofStoreError,
    ProofFailed,
    DepositIndexIncomplete,
    DepositMismatch,
    VerificationError,
    Unclassified,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_zk")]
pub struct ViaZKVerifierMetrics {
    #[metrics(buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    pub verification_time: Histogram<Duration>,

    /// Last valid l1 batch number.
    pub last_valid_l1_batch: Gauge<usize>,

    /// Errors
    pub errors: Counter,

    /// Polls that ended without a verdict, which a stuck batch repeats on every poll.
    #[metrics(labels = ["reason"])]
    pub non_verdicts: LabeledFamily<NoVerdictReason, Counter>,

    /// The batch awaiting a verdict from its selection until commit, or 0 when none is pending.
    pub blocked_l1_batch: Gauge<usize>,

    /// Committed development approvals without a proof.
    pub dev_accepted_batches: Counter,
}

#[vise::register]
pub static METRICS: vise::Global<ViaZKVerifierMetrics> = vise::Global::new();
