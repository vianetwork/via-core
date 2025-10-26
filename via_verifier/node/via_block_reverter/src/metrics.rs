use vise::{Counter, Metrics};

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_block_reverter")]
pub struct ViaVerifierBlockReverterMetrics {
    /// Counter to track execution of revert.
    pub revert: Counter,

    /// Errors
    pub errors: Counter,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierBlockReverterMetrics> = vise::Global::new();
