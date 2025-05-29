use via_btc_client::indexer::BitcoinInscriptionIndexer;
use via_btc_watch::BitcoinNetwork;
use via_verifier_btc_watch::VerifierBtcWatch;
use zksync_config::{
    configs::{via_btc_client::ViaBtcClientConfig, via_consensus::ViaGenesisConfig},
    ViaBtcWatchConfig,
};

use crate::{
    implementations::resources::{
        pools::{PoolResource, VerifierPool},
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
/// Responsible for initializing and running of [`VerifierBtcWatch`] component, that polls the Bitcoin node for the relevant events.
#[derive(Debug)]
pub struct VerifierBtcWatchLayer {
    via_genesis_config: ViaGenesisConfig,
    via_btc_client: ViaBtcClientConfig,
    btc_watch_config: ViaBtcWatchConfig,
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
    pub btc_indexer_resource: BtcIndexerResource,
    #[context(task)]
    pub btc_watch: VerifierBtcWatch,
}

impl VerifierBtcWatchLayer {
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
impl WiringLayer for VerifierBtcWatchLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "verifier_btc_watch_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let main_pool = input.master_pool.get().await?;
        let client = input.btc_client_resource.btc_sender.unwrap();
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

        let btc_watch = VerifierBtcWatch::new(
            self.btc_watch_config,
            indexer,
            main_pool,
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
impl Task for VerifierBtcWatch {
    fn id(&self) -> TaskId {
        "verifier_btc_watch".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
