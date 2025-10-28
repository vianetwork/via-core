use vise::{Counter, Gauge, Metrics};

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_fee_model")]
pub struct ViaFeeModelMetrics {
    /// The L1 gas price.
    pub l1_gas_price: Gauge<usize>,
    /// Counter to store the layer errors.
    pub errors: Counter,
}

#[vise::register]
pub static METRICS: vise::Global<ViaFeeModelMetrics> = vise::Global::new();
