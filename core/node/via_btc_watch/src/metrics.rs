use vise::{Counter, LabeledFamily, Metrics};

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_server_btc_watch")]
pub struct ViaBtcWatcherMetrics {
    #[metrics(labels = ["role", "address"])]
    pub system_wallets: LabeledFamily<(String, String), Counter, 2>,


    /// Deposit processed.
    pub deposit: Counter,

    /// Counter to store the layer errors.
    pub errors: Counter,
}

#[vise::register]
pub static METRICS: vise::Global<ViaBtcWatcherMetrics> = vise::Global::new();
