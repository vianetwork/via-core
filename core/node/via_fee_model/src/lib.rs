use std::{fmt::Debug, sync::Arc};

use async_trait::async_trait;
use l1_gas_price::gas_adjuster::ViaGasAdjuster;
pub use zksync_node_fee_model::BatchFeeModelInputProvider;
use zksync_types::{
    fee_model::{
        BaseTokenConversionRatio, BatchFeeInput, FeeModelConfig, FeeModelConfigV2, FeeParams,
        FeeParamsV2, PubdataIndependentBatchFeeModelInput,
    },
    U256,
};
use zksync_utils::ceil_div_u256;
mod l1_gas_price;

#[derive(Debug)]
pub struct ViaMainNodeFeeInputProvider {
    provider: Arc<ViaGasAdjuster>,
    fee_model_config: FeeModelConfigV2,
}

impl ViaMainNodeFeeInputProvider {
    pub fn new(provider: Arc<ViaGasAdjuster>, config: FeeModelConfig) -> anyhow::Result<Self> {
        match config {
            FeeModelConfig::V2(fee_model_config) => Ok(Self {
                provider,
                fee_model_config,
            }),
            FeeModelConfig::V1(_) => Err(anyhow::anyhow!("Via fee model must be inited using V2")),
        }
    }
}

#[async_trait]
impl BatchFeeModelInputProvider for ViaMainNodeFeeInputProvider {
    async fn get_batch_fee_input_scaled(
        &self,
        _l1_gas_price_scale_factor: f64,
        _l1_pubdata_price_scale_factor: f64,
    ) -> anyhow::Result<BatchFeeInput> {
        let config = self.get_fee_model_params();
        match config {
            FeeParams::V2(fee_params_v2) => Ok(BatchFeeInput::PubdataIndependent(
                clip_batch_fee_model_input_v2(compute_batch_fee_model_input_v2(
                    fee_params_v2,
                    _l1_gas_price_scale_factor,
                    _l1_pubdata_price_scale_factor,
                )),
            )),
            FeeParams::V1(_) => Err(anyhow::anyhow!("Via fee model must be inited using V2")),
        }
    }

    fn get_fee_model_params(&self) -> FeeParams {
        FeeParams::V2(FeeParamsV2::new(
            self.fee_model_config,
            self.provider.estimate_effective_gas_price(),
            self.provider.estimate_effective_pubdata_price(),
            BaseTokenConversionRatio::default(),
        ))
    }
}

/// Calculates the batch fee input based on the main node parameters.
/// This function uses the `V2` fee model, i.e. where the pubdata price does not include the proving costs.
fn compute_batch_fee_model_input_v2(
    params: FeeParamsV2,
    l1_gas_price_scale_factor: f64,
    l1_pubdata_price_scale_factor: f64,
) -> PubdataIndependentBatchFeeModelInput {
    let config = params.config();
    let l1_gas_price = params.l1_gas_price();
    let l1_pubdata_price = params.l1_pubdata_price();

    let FeeModelConfigV2 {
        minimal_l2_gas_price,
        compute_overhead_part,
        pubdata_overhead_part,
        batch_overhead_l1_gas,
        max_gas_per_batch,
        max_pubdata_per_batch,
    } = config;

    // Firstly, we scale the gas price and pubdata price in case it is needed.
    let l1_gas_price = (l1_gas_price as f64 * l1_gas_price_scale_factor) as u64;
    let l1_pubdata_price = (l1_pubdata_price as f64 * l1_pubdata_price_scale_factor) as u64;

    // While the final results of the calculations are not expected to have any overflows, the intermediate computations
    // might, so we use U256 for them.
    let l1_batch_overhead_sat = U256::from(l1_gas_price) * U256::from(batch_overhead_l1_gas);

    let fair_l2_gas_price = {
        // Firstly, we calculate which part of the overall overhead each unit of L2 gas should cover.
        let l1_batch_overhead_per_gas =
            ceil_div_u256(l1_batch_overhead_sat, U256::from(max_gas_per_batch));

        // Then, we multiply by the `compute_overhead_part` to get the overhead for the computation for each gas.
        // Also, this means that if we almost never close batches because of compute, the `compute_overhead_part` should be zero and so
        // it is possible that the computation costs include for no overhead.
        let gas_overhead_sat =
            (l1_batch_overhead_per_gas.as_u64() as f64 * compute_overhead_part) as u64;

        // We sum up the minimal L2 gas price (i.e. the raw prover/compute cost of a single L2 gas) and the overhead for batch being closed.
        minimal_l2_gas_price + gas_overhead_sat
    };

    let fair_pubdata_price = {
        // Firstly, we calculate which part of the overall overhead each pubdata byte should cover.
        let l1_batch_overhead_per_pubdata =
            ceil_div_u256(l1_batch_overhead_sat, U256::from(max_pubdata_per_batch));

        // Then, we multiply by the `pubdata_overhead_part` to get the overhead for each pubdata byte.
        // Also, this means that if we almost never close batches because of pubdata, the `pubdata_overhead_part` should be zero and so
        // it is possible that the pubdata costs include no overhead.
        let pubdata_overhead_sat =
            (l1_batch_overhead_per_pubdata.as_u64() as f64 * pubdata_overhead_part) as u64;

        // We sum up the raw L1 pubdata price (i.e. the expected price of publishing a single pubdata byte) and the overhead for batch being closed.
        l1_pubdata_price + pubdata_overhead_sat
    };

    PubdataIndependentBatchFeeModelInput {
        l1_gas_price,
        fair_l2_gas_price,
        fair_pubdata_price,
    }
}

/// Bootloader places limitations on fair_l2_gas_price and fair_pubdata_price.
/// (MAX_ALLOWED_FAIR_L2_GAS_PRICE and MAX_ALLOWED_FAIR_PUBDATA_PRICE in bootloader code respectively)
/// Server needs to clip this prices in order to allow chain continues operation at a loss. The alternative
/// would be to stop accepting the transactions until the conditions improve.
/// TODO (PE-153): to be removed when bootloader limitation is removed
fn clip_batch_fee_model_input_v2(
    fee_model: PubdataIndependentBatchFeeModelInput,
) -> PubdataIndependentBatchFeeModelInput {
    /// MAX_ALLOWED_FAIR_L2_GAS_PRICE
    const MAXIMUM_L2_GAS_PRICE: u64 = 20;
    /// MAX_ALLOWED_FAIR_PUBDATA_PRICE
    const MAXIMUM_PUBDATA_PRICE: u64 = 1_000_000_000_000_000;
    PubdataIndependentBatchFeeModelInput {
        l1_gas_price: fee_model.l1_gas_price,
        fair_l2_gas_price: if fee_model.fair_l2_gas_price < MAXIMUM_L2_GAS_PRICE {
            fee_model.fair_l2_gas_price
        } else {
            tracing::warn!(
                "Fair l2 gas price {} exceeds maximum. Limitting to {}",
                fee_model.fair_l2_gas_price,
                MAXIMUM_L2_GAS_PRICE
            );
            MAXIMUM_L2_GAS_PRICE
        },
        fair_pubdata_price: if fee_model.fair_pubdata_price < MAXIMUM_PUBDATA_PRICE {
            fee_model.fair_pubdata_price
        } else {
            tracing::warn!(
                "Fair pubdata price {} exceeds maximum. Limitting to {}",
                fee_model.fair_pubdata_price,
                MAXIMUM_PUBDATA_PRICE
            );
            MAXIMUM_PUBDATA_PRICE
        },
    }
}

#[derive(Debug)]
pub struct ViaApiFeeInputProvider {
    inner: Arc<dyn BatchFeeModelInputProvider>,
}

impl ViaApiFeeInputProvider {
    pub fn new(inner: Arc<dyn BatchFeeModelInputProvider>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl BatchFeeModelInputProvider for ViaApiFeeInputProvider {
    async fn get_batch_fee_input_scaled(
        &self,
        _l1_gas_price_scale_factor: f64,
        _l1_pubdata_price_scale_factor: f64,
    ) -> anyhow::Result<BatchFeeInput> {
        if let FeeParams::V2(params_v2) = self.inner.get_fee_model_params() {
            return Ok(BatchFeeInput::pubdata_independent(
                params_v2.l1_gas_price(),
                params_v2.l1_gas_price(),
                params_v2.l1_pubdata_price(),
            ));
        }
        Err(anyhow::Error::msg("Via batch fee must be v2"))
    }

    fn get_fee_model_params(&self) -> FeeParams {
        self.inner.get_fee_model_params()
    }
}

/// Mock [`BatchFeeModelInputProvider`] implementation that returns a constant value.
/// Intended to be used in tests only.
#[derive(Debug)]
pub struct MockBatchFeeParamsProvider(pub FeeParams);

impl Default for MockBatchFeeParamsProvider {
    fn default() -> Self {
        Self(FeeParams::sensible_v1_default())
    }
}

#[async_trait]
impl BatchFeeModelInputProvider for MockBatchFeeParamsProvider {
    fn get_fee_model_params(&self) -> FeeParams {
        self.0
    }
}
