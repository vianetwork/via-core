use via_withdrawal_service::coordinator;
use zksync_config::configs::via_verifier::ViaVerifierConfig;

use crate::{
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for contract verification
///
/// Responsible for initialization of the coordinator server.
#[derive(Debug)]
pub struct ViaCoordinatorApiLayer {
    pub config: ViaVerifierConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub coordinator_api_task: ViaCoordinatorApiTask,
}

#[async_trait::async_trait]
impl WiringLayer for ViaCoordinatorApiLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_coordinator_api_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let coordinator_api_task = ViaCoordinatorApiTask {
            config: self.config,
        };
        Ok(Output {
            coordinator_api_task,
        })
    }
}

#[derive(Debug)]
pub struct ViaCoordinatorApiTask {
    pub config: ViaVerifierConfig,
}

#[async_trait::async_trait]
impl Task for ViaCoordinatorApiTask {
    fn id(&self) -> TaskId {
        "via_coordinator_api".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        Ok(())
    }
}
