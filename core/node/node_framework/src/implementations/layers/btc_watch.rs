use via_btc_watch::{BitcoinNetwork, BtcWatch};
use zksync_config::BtcWatchConfig;

use crate::{
    implementations::resources::pools::{MasterPool, PoolResource},
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
    // TODO: divide into multiple configs
    btc_watch_config: BtcWatchConfig,
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
    pub btc_watch: BtcWatch,
}

impl BtcWatchLayer {
    pub fn new(btc_watch_config: BtcWatchConfig) -> Self {
        Self { btc_watch_config }
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
        let network = BitcoinNetwork::from_core_arg(self.btc_watch_config.network())
            .map_err(|_| WiringError::Configuration("Wrong network in config".to_string()))?;
        let bootstrap_txids = self
            .btc_watch_config
            .bootstrap_txids()
            .iter()
            .map(|txid| {
                txid.parse()
                    .map_err(|_| WiringError::Configuration("Wrong txid in config".to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let btc_watch = BtcWatch::new(
            self.btc_watch_config.rpc_url(),
            network,
            bootstrap_txids,
            main_pool,
            self.btc_watch_config.poll_interval(),
        )
        .await?;

        Ok(Output { btc_watch })
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
