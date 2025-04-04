use via_btc_client::inscriber::Inscriber;
use via_btc_sender::btc_inscription_aggregator::ViaBtcInscriptionAggregator;
use zksync_config::{
    configs::{
        via_btc_client::ViaBtcClientConfig, via_secrets::ViaL1Secrets, via_wallets::ViaWallet,
    },
    ViaBtcSenderConfig,
};

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
    via_btc_client: ViaBtcClientConfig,
    config: ViaBtcSenderConfig,
    wallet: ViaWallet,
    secrets: ViaL1Secrets,
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
    pub fn new(
        via_btc_client: ViaBtcClientConfig,
        config: ViaBtcSenderConfig,
        wallet: ViaWallet,
        secrets: ViaL1Secrets,
    ) -> Self {
        Self {
            via_btc_client,
            config,
            wallet,
            secrets,
        }
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
        // Get resources.
        let master_pool = input.master_pool.get().await.unwrap();

        let inscriber = Inscriber::new(
            self.secrets.rpc_url.expose_str(),
            self.via_btc_client.network(),
            self.secrets.auth_node(),
            &self.wallet.private_key,
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
