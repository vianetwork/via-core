use vise::{Counter, Gauge, LabeledFamily, Metrics};

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_btc_watch")]
pub struct ViaVerifierBtcWatcherMetrics {
    #[metrics(labels = ["role", "address"])]
    pub system_wallets: LabeledFamily<(String, String), Counter, 2>,


    /// Deposit processed.
    pub deposit: Counter,

    /// Withdrawal confirmed.
    pub withdrawal_confirmed: Counter,

    /// Errors
    pub errors: Counter,

    /// 1 while the verifier has stopped reading Bitcoin because a proof's parent batch is missing, otherwise 0.
    /// A restart does not help until an operator restores the missing proof history.
    pub proof_continuity_blocked: Gauge,
}

#[vise::register]
pub static METRICS: vise::Global<ViaVerifierBtcWatcherMetrics> = vise::Global::new();
