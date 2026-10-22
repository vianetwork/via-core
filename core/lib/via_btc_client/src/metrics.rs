use std::time::Duration;

use vise::{Buckets, Counter, EncodeLabelSet, Family, Histogram, Metrics, Unit};

#[derive(Debug, Clone, PartialEq, Eq, Hash, EncodeLabelSet)]
pub struct RpcMethodLabel {
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, EncodeLabelSet)]
pub struct RpcCallLabels {
    pub method: String,
    pub component: &'static str,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_btc_client")]
pub struct ViaBtcClientMetrics {
    /// Duration of each RPC attempt, excluding admission wait and retry backoff.
    #[metrics(buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    pub rpc_duration: Family<RpcCallLabels, Histogram<Duration>>,

    /// Number of RPC errors encountered, by method and error type
    pub rpc_errors: Family<RpcMethodLabel, Counter>,

    /// Number of RPC errors encountered, by method and error type
    pub rpc_max_retries_exceeded: Family<RpcMethodLabel, Counter>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaBtcClientMetrics> = vise::Global::new();
