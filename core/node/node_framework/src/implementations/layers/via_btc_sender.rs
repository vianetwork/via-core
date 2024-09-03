use via_btc_sender::{BitcoinNetwork, BtcSender};
use zksync_config::ViaBtcSenderConfig;

use crate::{
    implementations::resources::pools::{MasterPool, PoolResource},
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

#[derive(Debug)]
pub struct BtcSenderLayer {
    btc_sender_config: ViaBtcSenderConfig,
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
    pub btc_sender: BtcSender,
}

impl BtcSenderLayer {
    pub fn new(btc_sender_config: ViaBtcSenderConfig) -> Self {
        Self { btc_sender_config }
    }
}

#[async_trait::async_trait]
impl WiringLayer for BtcSenderLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "btc_sender_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let _main_pool = input.master_pool.get().await?;
        let _network = BitcoinNetwork::from_core_arg(self.btc_sender_config.network())
            .map_err(|_| WiringError::Configuration("Wrong network in config".to_string()))?;

        todo!("Implement BtcSenderLayer")
    }
}

#[async_trait::async_trait]
impl Task for BtcSender {
    fn id(&self) -> TaskId {
        "btc_sender".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
