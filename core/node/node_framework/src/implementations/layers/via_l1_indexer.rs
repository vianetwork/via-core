use via_btc_client::indexer::BitcoinInscriptionIndexer;
use via_indexer::L1Indexer;
use zksync_config::{configs::via_bridge::ViaBridgeConfig, ViaBtcWatchConfig};

use crate::{
    implementations::resources::{
        pools::{IndexerPool, PoolResource},
        via_btc_client::BtcClientResource,
        via_btc_indexer::BtcIndexerResource,
        via_system_wallet::ViaSystemWalletsResource,
    },
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

#[derive(Debug)]
pub struct L1IndexerLayer {
    via_bridge_config: ViaBridgeConfig,
    btc_watch_config: ViaBtcWatchConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<IndexerPool>,
    pub btc_client_resource: BtcClientResource,
    pub system_wallets_resource: ViaSystemWalletsResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub btc_indexer_resource: BtcIndexerResource,
    #[context(task)]
    pub l1_indexer: L1Indexer,
}

impl L1IndexerLayer {
    pub fn new(via_bridge_config: ViaBridgeConfig, btc_watch_config: ViaBtcWatchConfig) -> Self {
        Self {
            via_bridge_config,
            btc_watch_config,
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for L1IndexerLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "l1_indexer_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let main_pool = input.master_pool.get().await?;
        let client = input.btc_client_resource.bridge.unwrap();
        let system_wallets = input.system_wallets_resource.0;
        let indexer = BitcoinInscriptionIndexer::new(client.clone(), system_wallets);
        let btc_indexer_resource = BtcIndexerResource::from(indexer.clone());

        let l1_indexer = L1Indexer::new(
            self.btc_watch_config,
            self.via_bridge_config,
            indexer,
            client,
            main_pool,
        )
        .await?;

        Ok(Output {
            btc_indexer_resource,
            l1_indexer,
        })
    }
}

#[async_trait::async_trait]
impl Task for L1Indexer {
    fn id(&self) -> TaskId {
        "l1_indexer".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
