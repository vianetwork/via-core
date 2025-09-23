use via_btc_client::indexer::BitcoinInscriptionIndexer;
use via_verifier_btc_watch::VerifierBtcWatch;
use via_verifier_storage_init::wallets::ViaWalletsInitializer;
use zksync_config::{configs::via_bridge::ViaBridgeConfig, ViaBtcWatchConfig};

use crate::{
    implementations::resources::{
        pools::{PoolResource, VerifierPool},
        via_btc_client::BtcClientResource,
        via_btc_indexer::BtcIndexerResource,
        via_system_wallet::ViaSystemWalletsResource,
    },
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext, StopReceiver, Task, TaskId,
};

/// Wiring layer for bitcoin watcher
///
/// Responsible for initializing and running of [`VerifierBtcWatch`] component, that polls the Bitcoin node for the relevant events.
#[derive(Debug)]
pub struct VerifierBtcWatchLayer {
    pub via_bridge_config: ViaBridgeConfig,
    pub via_btc_watch_config: ViaBtcWatchConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<VerifierPool>,
    pub btc_client_resource: BtcClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub system_wallets_resource: ViaSystemWalletsResource,
    pub btc_indexer_resource: BtcIndexerResource,
    #[context(task)]
    pub btc_watch: VerifierBtcWatch,
}

#[async_trait::async_trait]
impl WiringLayer for VerifierBtcWatchLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_verifier_btc_watch_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let main_pool = input.master_pool.get().await?;
        let client = input.btc_client_resource.btc_sender.unwrap();

        let system_wallets = ViaWalletsInitializer::load_system_wallets(main_pool.clone()).await?;
        let system_wallets_resource = ViaSystemWalletsResource::from(system_wallets);

        let indexer =
            BitcoinInscriptionIndexer::new(client.clone(), system_wallets_resource.0.clone());

        let btc_indexer_resource = BtcIndexerResource::from(indexer.clone());

        let btc_watch = VerifierBtcWatch::new(
            self.via_btc_watch_config,
            indexer,
            client,
            main_pool,
            self.via_bridge_config.zk_agreement_threshold,
        )
        .await?;

        Ok(Output {
            system_wallets_resource,
            btc_indexer_resource,
            btc_watch,
        })
    }
}

#[async_trait::async_trait]
impl Task for VerifierBtcWatch {
    fn id(&self) -> TaskId {
        "via_verifier_btc_watch".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
