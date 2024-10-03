use std::sync::Arc;

use via_fee_model::{ViaApiFeeInputProvider, ViaMainNodeFeeInputProvider};
use zksync_config::configs::chain::StateKeeperConfig;
use zksync_types::fee_model::FeeModelConfig;

use crate::{
    implementations::resources::fee_input::{ApiFeeInputResource, SequencerFeeInputResource},
    wiring_layer::{WiringError, WiringLayer},
    IntoContext,
};

/// Wiring layer for L1 gas interfaces.
/// Adds several resources that depend on L1 gas price.
#[derive(Debug)]
pub struct ViaL1GasLayer {
    state_keeper_config: StateKeeperConfig,
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
    type Input = ();
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_l1_gas_layer"
    }

    async fn wire(self, _input: Self::Input) -> Result<Self::Output, WiringError> {
        let main_fee_input_provider = Arc::new(ViaMainNodeFeeInputProvider::new(
            FeeModelConfig::from_state_keeper_config(&self.state_keeper_config),
        )?);

        let api_fee_input_provider =
            Arc::new(ViaApiFeeInputProvider::new(main_fee_input_provider.clone()));

        Ok(Output {
            sequencer_fee_input: main_fee_input_provider.into(),
            api_fee_input: api_fee_input_provider.into(),
        })
    }
}
