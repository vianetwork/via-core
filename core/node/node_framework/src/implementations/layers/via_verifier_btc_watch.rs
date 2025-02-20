use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::NodeAuth};
use via_btc_watch::BitcoinNetwork;
use via_verifier_btc_watch::VerifierBtcWatch;
use zksync_config::ViaBtcWatchConfig;

use crate::{
    implementations::resources::{
        pools::{PoolResource, VerifierPool},
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
    // TODO: divide into multiple configs
    btc_watch_config: ViaBtcWatchConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<VerifierPool>,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub btc_indexer_resource: BtcIndexerResource,
    #[context(task)]
    pub btc_watch: VerifierBtcWatch,
}

impl VerifierBtcWatchLayer {
    pub fn new(btc_watch_config: ViaBtcWatchConfig) -> Self {
        Self { btc_watch_config }
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
        let network = BitcoinNetwork::from_core_arg(self.btc_watch_config.network())
            .map_err(|_| WiringError::Configuration("Wrong network in config".to_string()))?;
        let node_auth = NodeAuth::UserPass(
            self.btc_watch_config.rpc_user().to_string(),
            self.btc_watch_config.rpc_password().to_string(),
        );
        let bootstrap_txids = self
            .btc_watch_config
            .bootstrap_txids()
            .iter()
            .map(|txid| {
                txid.parse()
                    .map_err(|_| WiringError::Configuration("Wrong txid in config".to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let btc_blocks_lag = self.btc_watch_config.btc_blocks_lag();

        let indexer = BtcIndexerResource::from(
            BitcoinInscriptionIndexer::new(
                self.btc_watch_config.rpc_url(),
                network,
                node_auth.clone(),
                bootstrap_txids.clone(),
            )
            .await
            .map_err(|e| WiringError::Internal(e.into()))?,
        );
        let btc_watch = VerifierBtcWatch::new(
            self.btc_watch_config.rpc_url(),
            network,
            node_auth,
            self.btc_watch_config.confirmations_for_btc_msg,
            bootstrap_txids,
            main_pool,
            self.btc_watch_config.poll_interval(),
            btc_blocks_lag,
            self.btc_watch_config.actor_role(),
        )
        .await?;

        Ok(Output {
            btc_indexer_resource: indexer,
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
