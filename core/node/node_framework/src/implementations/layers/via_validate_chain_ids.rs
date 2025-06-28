use via_indexer::BitcoinNetwork;
use zksync_node_sync::via_validate_chain_ids_task::ValidateChainIdsTask;
use zksync_types::L2ChainId;

use crate::{
    implementations::resources::{
        main_node_client::MainNodeClientResource, via_btc_client::BtcClientResource,
    },
    service::StopReceiver,
    task::{Task, TaskId, TaskKind},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for chain ID validation precondition for external node.
/// Ensures that chain IDs are consistent locally, on main node, and on the settlement layer.
#[derive(Debug)]
pub struct ViaValidateChainIdsLayer {
    network: BitcoinNetwork,
    l2_chain_id: L2ChainId,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub btc_client_resource: BtcClientResource,
    pub main_node_client: MainNodeClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub task: ValidateChainIdsTask,
}

impl ViaValidateChainIdsLayer {
    pub fn new(network: BitcoinNetwork, l2_chain_id: L2ChainId) -> Self {
        Self {
            network,
            l2_chain_id,
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaValidateChainIdsLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_validate_chain_ids_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let MainNodeClientResource(main_node_client) = input.main_node_client;
        let btc_client = input.btc_client_resource.default;
        let task =
            ValidateChainIdsTask::new(self.network, self.l2_chain_id, btc_client, main_node_client);

        Ok(Output { task })
    }
}

#[async_trait::async_trait]
impl Task for ValidateChainIdsTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Precondition
    }

    fn id(&self) -> TaskId {
        "validate_chain_ids".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run_once(stop_receiver.0).await
    }
}
