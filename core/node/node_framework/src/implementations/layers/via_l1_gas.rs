use std::sync::Arc;

use via_fee_model::{ViaApiFeeInputProvider, ViaMainNodeFeeInputProvider};
use zksync_config::configs::chain::StateKeeperConfig;
use zksync_node_framework_derive::FromContext;
use zksync_types::fee_model::FeeModelConfig;

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
    state_keeper_config: StateKeeperConfig,
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
    pub fn new(state_keeper_config: StateKeeperConfig) -> Self {
        Self {
            state_keeper_config,
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
            FeeModelConfig::from_state_keeper_config(&self.state_keeper_config),
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
