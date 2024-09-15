use via_btc_client::{client::BitcoinClient, inscriber::Inscriber, types::NodeAuth};
use via_btc_sender::btc_inscription_manager::ViaBtcInscriptionManager;
use via_btc_watch::BitcoinNetwork;
use zksync_config::ViaBtcSenderConfig;

use crate::{
    implementations::resources::pools::{MasterPool, PoolResource},
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for `eth_txs` managing
///
/// Responsible for initialization and running [`ViaBtcInscriptionTxManager`] component, that manages sending
/// of `inscriptions_requests`(such as `CommitL1Block` or `CommitProof`) to L1.
///
/// ## Requests resources
///
/// - `PoolResource<MasterPool>`
///
/// ## Adds tasks
///
/// - `ViaBtcInscriptionManager`
#[derive(Debug)]
pub struct ViaInscriptionManagerLayer {
    pub config: ViaBtcSenderConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<MasterPool>,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub via_btc_inscription_manager: ViaBtcInscriptionManager,
}

impl ViaInscriptionManagerLayer {
    pub fn new(config: ViaBtcSenderConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaInscriptionManagerLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "eth_tx_manager_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        // Get resources.
        let master_pool = input.master_pool.get().await.unwrap();

        let network = BitcoinNetwork::from_core_arg(self.config.network())
            .map_err(|_| WiringError::Configuration("Wrong network in config".to_string()))?;

        // Todo: update node auth
        let client = BitcoinClient::new(self.config.rpc_url(), network, NodeAuth::None).unwrap();

        let inscriber = Inscriber::new(
            self.config.rpc_url(),
            network,
            NodeAuth::None,
            self.config.private_key(),
            None,
        )
        .await
        .unwrap();

        let via_btc_inscription_manager =
            ViaBtcInscriptionManager::new(client, inscriber, master_pool, self.config)
                .await
                .unwrap();

        Ok(Output {
            via_btc_inscription_manager,
        })
    }
}

#[async_trait::async_trait]
impl Task for ViaBtcInscriptionManager {
    fn id(&self) -> TaskId {
        "via_btc_inscription_manager".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
