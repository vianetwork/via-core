use via_verifier_block_reverter::ViaVerifierBlockReverter;
use zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig;

use crate::{
    implementations::resources::pools::{PoolResource, VerifierPool},
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

#[derive(Debug)]
pub struct VerifierBlockReverterLayer {
    config: ViaReorgDetectorConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<VerifierPool>,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub block_reverter: ViaVerifierBlockReverter,
}

impl VerifierBlockReverterLayer {
    pub fn new(config: ViaReorgDetectorConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl WiringLayer for VerifierBlockReverterLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "verifier_block_reverter"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let main_pool = input.master_pool.get().await?;
        let block_reverter = ViaVerifierBlockReverter::new(main_pool, self.config);

        Ok(Output { block_reverter })
    }
}

#[async_trait::async_trait]
impl Task for ViaVerifierBlockReverter {
    fn id(&self) -> TaskId {
        "verifier_block_reverter".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
