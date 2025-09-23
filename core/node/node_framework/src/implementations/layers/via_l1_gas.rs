use std::sync::Arc;

use via_fee_model::{ViaApiFeeInputProvider, ViaMainNodeFeeInputProvider};
use zksync_config::configs::chain::{FeeModelVersion, StateKeeperConfig};
use zksync_node_framework_derive::FromContext;
use zksync_types::fee_model::{FeeModelConfig, FeeModelConfigV1, FeeModelConfigV2};

use crate::{
    implementations::resources::{
        fee_input::{ApiFeeInputResource, SequencerFeeInputResource},
        pools::{MasterPool, PoolResource},
        via_gas_adjuster::ViaGasAdjusterResource,
    },
    wiring_layer::{WiringError, WiringLayer},
    IntoContext,
};

/// Wiring layer for L1 gas interfaces.
/// Adds several resources that depend on L1 gas price.
#[derive(Debug)]
pub struct ViaL1GasLayer {
    fee_model_config: FeeModelConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<MasterPool>,
    pub gas_adjuster: ViaGasAdjusterResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub sequencer_fee_input: SequencerFeeInputResource,
    pub api_fee_input: ApiFeeInputResource,
}

impl ViaL1GasLayer {
    pub fn new(state_keeper_config: &StateKeeperConfig) -> Self {
        Self {
            fee_model_config: Self::map_config(state_keeper_config),
        }
    }

    fn map_config(state_keeper_config: &StateKeeperConfig) -> FeeModelConfig {
        match state_keeper_config.fee_model_version {
            FeeModelVersion::V1 => FeeModelConfig::V1(FeeModelConfigV1 {
                minimal_l2_gas_price: state_keeper_config.minimal_l2_gas_price,
            }),
            FeeModelVersion::V2 => FeeModelConfig::V2(FeeModelConfigV2 {
                minimal_l2_gas_price: state_keeper_config.minimal_l2_gas_price,
                compute_overhead_part: state_keeper_config.compute_overhead_part,
                pubdata_overhead_part: state_keeper_config.pubdata_overhead_part,
                batch_overhead_l1_gas: state_keeper_config.batch_overhead_l1_gas,
                max_gas_per_batch: state_keeper_config.max_gas_per_batch,
                max_pubdata_per_batch: state_keeper_config.max_pubdata_per_batch,
            }),
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaL1GasLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_l1_gas_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let main_fee_input_provider = Arc::new(ViaMainNodeFeeInputProvider::new(
            input.gas_adjuster.0.clone(),
            self.fee_model_config,
        )?);

        let main_pool = input.master_pool.get().await?;

        let api_fee_input_provider = Arc::new(ViaApiFeeInputProvider::new(
            main_fee_input_provider.clone(),
            main_pool,
        ));

        Ok(Output {
            sequencer_fee_input: main_fee_input_provider.into(),
            api_fee_input: api_fee_input_provider.into(),
        })
    }
}
