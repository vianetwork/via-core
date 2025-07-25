use std::sync::Arc;

use via_fee_model::l1_gas_price::main_node_fetcher::ViaMainNodeFeeParamsFetcher;

use crate::{
    implementations::resources::{
        fee_input::{ApiFeeInputResource, SequencerFeeInputResource},
        main_node_client::MainNodeClientResource,
    },
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for main node fee params fetcher -- a fee input resource used on
/// the external node.
#[derive(Debug)]
pub struct ViaMainNodeFeeParamsFetcherLayer;

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub main_node_client: MainNodeClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub sequencer_fee_input: SequencerFeeInputResource,
    pub api_fee_input: ApiFeeInputResource,
    #[context(task)]
    pub fetcher: ViaMainNodeFeeParamsFetcherTask,
}

#[async_trait::async_trait]
impl WiringLayer for ViaMainNodeFeeParamsFetcherLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "main_node_fee_params_fetcher_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let MainNodeClientResource(main_node_client) = input.main_node_client;
        let fetcher = Arc::new(ViaMainNodeFeeParamsFetcher::new(main_node_client));
        Ok(Output {
            sequencer_fee_input: fetcher.clone().into(),
            api_fee_input: fetcher.clone().into(),
            fetcher: ViaMainNodeFeeParamsFetcherTask { fetcher },
        })
    }
}

#[derive(Debug)]
pub struct ViaMainNodeFeeParamsFetcherTask {
    fetcher: Arc<ViaMainNodeFeeParamsFetcher>,
}

#[async_trait::async_trait]
impl Task for ViaMainNodeFeeParamsFetcherTask {
    fn id(&self) -> TaskId {
        "main_node_fee_params_fetcher".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        self.fetcher.run(stop_receiver.0).await
    }
}
