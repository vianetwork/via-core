use via_btc_client::indexer::BitcoinInscriptionIndexer;
use via_btc_watch::{BitcoinNetwork, BtcWatch};
use zksync_config::{
    configs::{via_btc_client::ViaBtcClientConfig, via_consensus::ViaGenesisConfig},
    ViaBtcWatchConfig,
};

use crate::{
    implementations::resources::{
        pools::{MasterPool, PoolResource},
        via_btc_client::BtcClientResource,
        via_btc_indexer::BtcIndexerResource,
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
    via_genesis_config: ViaGenesisConfig,
    via_btc_client: ViaBtcClientConfig,
    btc_watch_config: ViaBtcWatchConfig,
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
    pub btc_indexer_resource: BtcIndexerResource,
    #[context(task)]
    pub btc_watch: BtcWatch,
}

impl BtcWatchLayer {
    pub fn new(
        via_genesis_config: ViaGenesisConfig,
        via_btc_client: ViaBtcClientConfig,
        btc_watch_config: ViaBtcWatchConfig,
    ) -> Self {
        Self {
            via_genesis_config,
            via_btc_client,
            btc_watch_config,
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for BtcWatchLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "btc_watch_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let main_pool = input.master_pool.get().await?;
        let client = input.btc_client_resource.0;
        let indexer = BitcoinInscriptionIndexer::new(
            client,
            self.via_btc_client.clone(),
            self.via_genesis_config.bootstrap_txids()?,
        )
        .await
        .map_err(|e| WiringError::Internal(e.into()))?;
        let btc_indexer_resource = BtcIndexerResource::from(indexer.clone());
        // We should not set block_confirmations to 0 for mainnet,
        // because we need to wait for some confirmations to be sure that the transaction is included in a block.
        if self.via_btc_client.network() == BitcoinNetwork::Bitcoin
            && self.btc_watch_config.block_confirmations == 0
        {
            return Err(WiringError::Configuration(
                "block_confirmations cannot be 0 for mainnet".into(),
            ));
        }

        let btc_watch = BtcWatch::new(
            self.btc_watch_config,
            indexer,
            main_pool,
            self.via_genesis_config.bridge_address()?,
            self.via_genesis_config.zk_agreement_threshold,
        )
        .await?;

        Ok(Output {
            btc_indexer_resource,
            btc_watch,
        })
    }
}

#[async_trait::async_trait]
impl Task for BtcWatch {
    fn id(&self) -> TaskId {
        "btc_watch".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
