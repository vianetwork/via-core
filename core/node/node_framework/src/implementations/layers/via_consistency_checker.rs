use via_consistency_checker::ConsistencyChecker;

use crate::{
    implementations::resources::{
        healthcheck::AppHealthCheckResource,
        pools::{MasterPool, PoolResource},
        via_btc_client::BtcClientResource,
        via_system_wallet::ViaSystemWalletsResource,
    },
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for the `ConsistencyChecker` (used by the external node).
#[derive(Debug)]
pub struct ViaConsistencyCheckerLayer {
    max_batches_to_recheck: u32,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub system_wallets_resource: ViaSystemWalletsResource,
    pub btc_client_resource: BtcClientResource,
    pub master_pool: PoolResource<MasterPool>,
    #[context(default)]
    pub app_health: AppHealthCheckResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub consistency_checker: ConsistencyChecker,
}

impl ViaConsistencyCheckerLayer {
    pub fn new(max_batches_to_recheck: u32) -> ViaConsistencyCheckerLayer {
        Self {
            max_batches_to_recheck,
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaConsistencyCheckerLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_consistency_checker_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        // Get resources.
        let btc_client = input.btc_client_resource.default;

        let singleton_pool = input.master_pool.get_singleton().await?;

        let consistency_checker = ConsistencyChecker::new(
            input.system_wallets_resource.0.sequencer.clone(),
            "celestia".into(),
            btc_client,
            self.max_batches_to_recheck,
            singleton_pool,
        )
        .map_err(WiringError::Internal)?;
        input
            .app_health
            .0
            .insert_component(consistency_checker.health_check().clone())
            .map_err(WiringError::internal)?;

        Ok(Output {
            consistency_checker,
        })
    }
}

#[async_trait::async_trait]
impl Task for ConsistencyChecker {
    fn id(&self) -> TaskId {
        "via_consistency_checker".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
