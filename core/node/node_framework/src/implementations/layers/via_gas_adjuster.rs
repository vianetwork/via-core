use std::sync::Arc;

use anyhow::Context;
use via_btc_client::{inscriber::Inscriber, types::NodeAuth};
use via_btc_watch::BitcoinNetwork;
use via_fee_model::ViaGasAdjuster;
use zksync_config::{GasAdjusterConfig, ViaBtcSenderConfig};

use crate::{
    implementations::resources::via_gas_adjuster::ViaGasAdjusterResource,
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for sequencer L1 gas interfaces.
/// Adds several resources that depend on L1 gas price.
#[derive(Debug)]
pub struct ViaGasAdjusterLayer {
    gas_adjuster_config: GasAdjusterConfig,
    config: ViaBtcSenderConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub gas_adjuster: ViaGasAdjusterResource,
    /// Only runs if someone uses the resources listed above.
    #[context(task)]
    pub gas_adjuster_task: ViaGasAdjusterTask,
}

impl ViaGasAdjusterLayer {
    pub fn new(gas_adjuster_config: GasAdjusterConfig, config: ViaBtcSenderConfig) -> Self {
        Self {
            gas_adjuster_config,
            config,
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaGasAdjusterLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_gas_adjuster_layer"
    }

    async fn wire(self, _: Self::Input) -> Result<Self::Output, WiringError> {
        let network = BitcoinNetwork::from_core_arg(self.config.network())
            .map_err(|_| WiringError::Configuration("Wrong network in config".to_string()))?;

        let inscriber = Inscriber::new(
            self.config.rpc_url(),
            network,
            NodeAuth::UserPass(
                self.config.rpc_user().to_string(),
                self.config.rpc_password().to_string(),
            ),
            self.config.private_key(),
            None,
        )
        .await
        .context("Init inscriber")?;

        let adjuster = ViaGasAdjuster::new(self.gas_adjuster_config, inscriber)
            .await
            .context("GasAdjuster::new()")?;
        let gas_adjuster = Arc::new(adjuster);

        Ok(Output {
            gas_adjuster: gas_adjuster.clone().into(),
            gas_adjuster_task: ViaGasAdjusterTask { gas_adjuster },
        })
    }
}

#[derive(Debug)]
pub struct ViaGasAdjusterTask {
    gas_adjuster: Arc<ViaGasAdjuster>,
}

#[async_trait::async_trait]
impl Task for ViaGasAdjusterTask {
    fn id(&self) -> TaskId {
        "via_gas_adjuster".into()
    }

    async fn run(self: Box<Self>, mut stop_receiver: StopReceiver) -> anyhow::Result<()> {
        // Gas adjuster layer is added to provide a resource for anyone to use, but it comes with
        // a support task. If nobody has used the resource, we don't need to run the support task.
        if Arc::strong_count(&self.gas_adjuster) == 1 {
            tracing::info!(
                "Via gas adjuster is not used by any other task, not running the support task"
            );
            stop_receiver.0.changed().await?;
            return Ok(());
        }

        self.gas_adjuster.run(stop_receiver.0).await
    }
}
