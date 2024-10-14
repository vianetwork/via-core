use std::{fmt::Debug, sync::Arc};

use async_trait::async_trait;
pub use zksync_node_fee_model::BatchFeeModelInputProvider;
use zksync_types::fee_model::{
    BaseTokenConversionRatio, BatchFeeInput, FeeModelConfig, FeeModelConfigV2, FeeParams,
    FeeParamsV2,
};

#[derive(Debug)]
pub struct ViaMainNodeFeeInputProvider {
    fee_model_config: FeeModelConfigV2,
}

impl ViaMainNodeFeeInputProvider {
    pub fn new(config: FeeModelConfig) -> anyhow::Result<Self> {
        match config {
            FeeModelConfig::V2(fee_model_config) => Ok(Self { fee_model_config }),
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
        Ok(BatchFeeInput::pubdata_independent(
            self.fee_model_config.minimal_l2_gas_price,
            self.fee_model_config.minimal_l2_gas_price,
            self.fee_model_config.max_pubdata_per_batch,
        ))
    }

    fn get_fee_model_params(&self) -> FeeParams {
        FeeParams::V2(FeeParamsV2::new(
            self.fee_model_config,
            self.fee_model_config.minimal_l2_gas_price,
            self.fee_model_config.max_pubdata_per_batch,
            BaseTokenConversionRatio::default(),
        ))
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
