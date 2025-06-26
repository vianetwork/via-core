use std::{fmt::Debug, sync::Arc};

use anyhow::Context;
use async_trait::async_trait;
pub use l1_gas_price::gas_adjuster::ViaGasAdjuster;
use zksync_dal::{ConnectionPool, Core, CoreDal};
pub use zksync_node_fee_model::BatchFeeModelInputProvider;
use zksync_types::fee_model::{
    BaseTokenConversionRatio, BatchFeeInput, FeeModelConfig, FeeModelConfigV2, FeeParams,
    FeeParamsV2, PubdataIndependentBatchFeeModelInput,
};

pub mod l1_gas_price;

#[async_trait]
pub trait ViaBaseTokenRatioProvider: Debug + Send + Sync + 'static {
    fn get_conversion_ratio_by_timestamp(
        &self,
        from_timestamp: u64,
        to_timestamp: u64,
    ) -> BaseTokenConversionRatio;
}

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
        if let FeeParams::V2(params_v2) = self.get_fee_model_params() {
            let fee = clip_batch_fee_model_input(compute_batch_fee_model_input(
                params_v2,
                _l1_gas_price_scale_factor,
                _l1_pubdata_price_scale_factor,
            ));
            return Ok(BatchFeeInput::PubdataIndependent(fee));
        }
        Err(anyhow::Error::msg("Via batch fee must be v2"))
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

#[derive(Debug)]
pub struct ViaApiFeeInputProvider {
    inner: Arc<dyn BatchFeeModelInputProvider>,
    connection_pool: ConnectionPool<Core>,
}

impl ViaApiFeeInputProvider {
    pub fn new(
        inner: Arc<dyn BatchFeeModelInputProvider>,
        connection_pool: ConnectionPool<Core>,
    ) -> Self {
        Self {
            inner,
            connection_pool,
        }
    }
}

#[async_trait]
impl BatchFeeModelInputProvider for ViaApiFeeInputProvider {
    async fn get_batch_fee_input_scaled(
        &self,
        l1_gas_price_scale_factor: f64,
        l1_pubdata_price_scale_factor: f64,
    ) -> anyhow::Result<BatchFeeInput> {
        let inner_input = self
            .inner
            .get_batch_fee_input_scaled(l1_gas_price_scale_factor, l1_pubdata_price_scale_factor)
            .await
            .context("cannot get batch fee input from base provider")?;
        let last_l2_block_params = self
            .connection_pool
            .connection_tagged("via_api_fee_input_provider")
            .await?
            .blocks_dal()
            .get_last_sealed_l2_block_header()
            .await?;

        Ok(last_l2_block_params
            .map(|header| inner_input.stricter(header.batch_fee_input))
            .unwrap_or(inner_input))
    }

    fn get_fee_model_params(&self) -> FeeParams {
        self.inner.get_fee_model_params()
    }
}

/// Calculates the batch fee input based on the main node parameters.
fn compute_batch_fee_model_input(
    params: FeeParamsV2,
    l1_gas_price_scale_factor: f64,
    l1_pubdata_price_scale_factor: f64,
) -> PubdataIndependentBatchFeeModelInput {
    let config = params.config();
    let l1_gas_price = params.l1_gas_price();
    let l1_pubdata_price = params.l1_pubdata_price();

    // Firstly, we scale the gas price and pubdata price in case it is needed.
    let l1_gas_price = (l1_gas_price as f64 * l1_gas_price_scale_factor) as u64;
    let l1_pubdata_price = (l1_pubdata_price as f64 * l1_pubdata_price_scale_factor) as u64;

    // Todo: rename "batch_overhead_l1_gas" to "total_inscription_gas_vbyte"
    let inscriptions_cost_satoshi = config.batch_overhead_l1_gas * l1_gas_price;
    // Scale the inscriptions_cost_satoshi to 18 decimals
    let gas_price_satoshi = inscriptions_cost_satoshi * 10_000_000_000 / config.max_gas_per_batch;
    // The "minimal_l2_gas_price" calculated from the operational cost to publish and verify block.
    let fair_l2_gas_price = gas_price_satoshi + config.minimal_l2_gas_price;

    PubdataIndependentBatchFeeModelInput {
        l1_gas_price,
        fair_l2_gas_price,
        fair_pubdata_price: l1_pubdata_price,
    }
}

/// Bootloader places limitations on fair_l2_gas_price and fair_pubdata_price.
/// (MAX_ALLOWED_FAIR_L2_GAS_PRICE and MAX_ALLOWED_FAIR_PUBDATA_PRICE in bootloader code respectively)
/// Server needs to clip this prices in order to allow chain continues operation at a loss. The alternative
/// would be to stop accepting the transactions until the conditions improve.
/// TODO (PE-153): to be removed when bootloader limitation is removed
fn clip_batch_fee_model_input(
    fee_model: PubdataIndependentBatchFeeModelInput,
) -> PubdataIndependentBatchFeeModelInput {
    /// MAX_ALLOWED_FAIR_L2_GAS_PRICE
    const MAXIMUM_L2_GAS_PRICE: u64 = 10_000_000_000_000;
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
