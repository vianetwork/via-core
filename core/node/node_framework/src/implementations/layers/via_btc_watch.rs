use via_btc_client::indexer::BitcoinInscriptionIndexer;
use via_btc_watch::BtcWatch;
use via_node_storage_init::wallets::ViaWalletsInitializer;
use zksync_config::{configs::via_bridge::ViaBridgeConfig, ViaBtcWatchConfig};

use crate::{
    implementations::resources::{
        pools::{MasterPool, PoolResource},
        via_btc_client::BtcClientResource,
        via_btc_indexer::BtcIndexerResource,
        via_system_wallet::ViaSystemWalletsResource,
    },
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for bitcoin watcher
///
/// Responsible for initializing and running of [`BtcWatch`] component, that polls the Bitcoin node for the relevant events.
#[derive(Debug)]
pub struct BtcWatchLayer {
    pub via_bridge_config: ViaBridgeConfig,
    pub via_btc_watch_config: ViaBtcWatchConfig,
    pub is_main_node: bool,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<MasterPool>,
    pub btc_client_resource: BtcClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub system_wallets_resource: ViaSystemWalletsResource,
    pub btc_indexer_resource: BtcIndexerResource,
    #[context(task)]
    pub btc_watch: BtcWatch,
}

#[async_trait::async_trait]
impl WiringLayer for BtcWatchLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_btc_watch_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let main_pool = input.master_pool.get().await?;
        let client = input.btc_client_resource.default;

        let system_wallets = ViaWalletsInitializer::load_system_wallets(main_pool.clone()).await?;
        let system_wallets_resource = ViaSystemWalletsResource::from(system_wallets);

        let indexer =
            BitcoinInscriptionIndexer::new(client.clone(), system_wallets_resource.0.clone());
        let btc_indexer_resource = BtcIndexerResource::from(indexer.clone());

        let btc_watch = BtcWatch::new(
            self.via_btc_watch_config,
            indexer,
            client,
            main_pool,
            self.via_bridge_config.zk_agreement_threshold,
            self.is_main_node,
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
impl Task for BtcWatch {
    fn id(&self) -> TaskId {
        "via_btc_watch".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
