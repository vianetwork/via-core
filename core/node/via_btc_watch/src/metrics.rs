//! Metrics for Via btc watcher.

use vise::{Counter, Metrics};

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_server_btc_watch")]
pub(super) struct ViaBtcWatcherMetrics {
    /// Number of times Bitcoin was polled.
    pub btc_poll: Counter,
}

#[vise::register]
pub(super) static METRICS: vise::Global<ViaBtcWatcherMetrics> = vise::Global::new();
