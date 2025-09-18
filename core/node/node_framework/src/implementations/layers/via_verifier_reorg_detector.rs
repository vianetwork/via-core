use via_verifier_reorg_detector::ViaVerifierReorgDetector;
use zksync_config::configs::via_reorg_detector::ViaReorgDetectorConfig;

use crate::{
    implementations::resources::{
        pools::{PoolResource, VerifierPool},
        via_btc_client::BtcClientResource,
    },
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext, StopReceiver, Task, TaskId,
};

#[derive(Debug)]
pub struct ViaVerifierReorgDetectorLayer {
    config: ViaReorgDetectorConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub pool: PoolResource<VerifierPool>,
    pub btc_client_resource: BtcClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub reorg_detector: ViaVerifierReorgDetector,
}

impl ViaVerifierReorgDetectorLayer {
    pub fn new(config: ViaReorgDetectorConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaVerifierReorgDetectorLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "Via_verifier_reorg_detector"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let client = input.btc_client_resource.default;
        let pool = input.pool.get().await?;

        let reorg_detector = ViaVerifierReorgDetector::new(self.config, pool, client.clone());

        Ok(Output { reorg_detector })
    }
}

#[async_trait::async_trait]
impl Task for ViaVerifierReorgDetector {
    fn id(&self) -> TaskId {
        "Via_verifier_reorg_detector".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
