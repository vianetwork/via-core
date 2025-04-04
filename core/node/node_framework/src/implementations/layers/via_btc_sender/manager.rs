use anyhow::Context;
use via_btc_client::inscriber::Inscriber;
use via_btc_sender::btc_inscription_manager::ViaBtcInscriptionManager;
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

/// Wiring layer for btc_sender to manage `inscriptions_requests`
///
/// Responsible for initialization and running [`ViaBtcInscriptionTxManager`] component. The layer is responsible
/// to process inscription requests and create btc transactions.
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
    pub via_btc_inscription_manager: ViaBtcInscriptionManager,
}

impl ViaInscriptionManagerLayer {
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
impl WiringLayer for ViaInscriptionManagerLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_btc_inscription_manager_layer"
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
        .with_context(|| "Error init inscriber")?;

        let via_btc_inscription_manager =
            ViaBtcInscriptionManager::new(inscriber, master_pool, self.config)
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
