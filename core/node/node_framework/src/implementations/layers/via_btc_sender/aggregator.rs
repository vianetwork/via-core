use via_btc_client::{
    inscriber::Inscriber,
    types::{BitcoinNetwork, NodeAuth},
};
use via_btc_sender::btc_inscription_aggregator::ViaBtcInscriptionAggregator;
use zksync_config::ViaBtcSenderConfig;

use crate::{
    implementations::resources::pools::{MasterPool, PoolResource},
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for btc_sender aggregating l1 batches into 'btc_inscriptions'
///
/// Responsible for initialization and running of [`ViaBtcInscriptionAggregator`], that create inscription requests
/// (such as `CommitL1Block` or `CommitProof`).
/// These `inscription_request` will be used as a queue for generating signed txs and will be sent later on L1.
///
/// ## Requests resources
///
/// - `PoolResource<MasterPool>`
///
/// ## Adds tasks
///
/// - `ViaBtcInscriptionAggregator`
#[derive(Debug)]
pub struct ViaBtcInscriptionAggregatorLayer {
    config: ViaBtcSenderConfig,
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
    pub via_btc_inscription_aggregator: ViaBtcInscriptionAggregator,
}

impl ViaBtcInscriptionAggregatorLayer {
    pub fn new(config: ViaBtcSenderConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaBtcInscriptionAggregatorLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_btc_inscription_request_aggregator_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        // // Get resources.
        let master_pool = input.master_pool.get().await.unwrap();

        let network = BitcoinNetwork::from_core_arg(self.config.network())
            .map_err(|_| WiringError::Configuration("Wrong network in config".to_string()))?;

        let inscriber = Inscriber::new(
            self.config.rpc_url(),
            network,
            NodeAuth::UserPass(
                self.config.rpc_user().to_string(),
                self.config.rpc_password().to_string(),
            ),
            self.config.private_key(),
            None,
        )
        .await
        .unwrap();

        let via_btc_inscription_aggregator =
            ViaBtcInscriptionAggregator::new(inscriber, master_pool, self.config).await?;

        Ok(Output {
            via_btc_inscription_aggregator,
        })
    }
}

#[async_trait::async_trait]
impl Task for ViaBtcInscriptionAggregator {
    fn id(&self) -> TaskId {
        "via_btc_inscription_aggregator".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
