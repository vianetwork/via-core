use vise::{Counter, EncodeLabelSet, Family, Metrics};

#[derive(Debug, Clone, PartialEq, Eq, Hash, EncodeLabelSet)]
pub struct RpcMethodLabel {
    pub method: String,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_btc_client")]
pub struct ViaBtcClientMetrics {
    /// Number of RPC errors encountered, by method and error type
    pub rpc_errors: Family<RpcMethodLabel, Counter>,

    /// Number of RPC errors encountered, by method and error type
    pub rpc_max_retries_exceeded: Family<RpcMethodLabel, Counter>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaBtcClientMetrics> = vise::Global::new();
