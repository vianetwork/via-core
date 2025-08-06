use std::sync::Arc;

use via_verifier_storage_init::ViaVerifierStorageInitializer;
use zksync_config::{configs::via_consensus::ViaGenesisConfig, GenesisConfig};

use crate::{
    implementations::resources::{
        pools::{PoolResource, VerifierPool},
        via_btc_client::BtcClientResource,
        via_indexer_wallet::ViaSystemWalletsResource,
    },
    task::TaskKind,
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext, StopReceiver, Task, TaskId,
};

/// Wiring layer for via verifier initialization.
#[derive(Debug)]
pub struct ViaVerifierInitLayer {
    pub via_genesis_config: ViaGenesisConfig,
    pub genesis: GenesisConfig,
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
    #[context(task)]
    pub initializer: ViaVerifierStorageInitializer,
    #[context(task)]
    pub precondition: ViaVerifierStorageInitializerPrecondition,
    pub system_wallets_resource: ViaSystemWalletsResource,
}

#[async_trait::async_trait]
impl WiringLayer for ViaVerifierInitLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_verifier_init_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let pool = input.master_pool.get().await?;
        let client = input.btc_client_resource.verifier.unwrap();

        let initializer =
            ViaVerifierStorageInitializer::new(self.via_genesis_config, self.genesis, pool, client);

        let system_wallets =
            ViaSystemWalletsResource(Arc::new(initializer.indexer_wallets().await.unwrap()));

        let system_wallets_resource = ViaSystemWalletsResource::from(system_wallets);

        let precondition = ViaVerifierStorageInitializerPrecondition(initializer.clone());
        Ok(Output {
            initializer,
            precondition,
            system_wallets_resource,
        })
    }
}

#[async_trait::async_trait]
impl Task for ViaVerifierStorageInitializer {
    fn kind(&self) -> TaskKind {
        TaskKind::UnconstrainedOneshotTask
    }

    fn id(&self) -> TaskId {
        "via_verifier_storage_initializer".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        tracing::info!("Starting the verifier storage initialization task");
        (*self).run(stop_receiver.0).await?;
        tracing::info!("Verifier storage initialization task completed");
        Ok(())
    }
}

/// Runs [`ViaVerifierStorageInitializer`] as a precondition, blocking
/// tasks from starting until the storage is initialized.
#[derive(Debug)]
pub struct ViaVerifierStorageInitializerPrecondition(ViaVerifierStorageInitializer);

#[async_trait::async_trait]
impl Task for ViaVerifierStorageInitializerPrecondition {
    fn kind(&self) -> TaskKind {
        TaskKind::Precondition
    }

    fn id(&self) -> TaskId {
        "via_verifier_storage_initializer_precondition".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        tracing::info!("Waiting for verifier storage to be initialized");
        let result = self.0.wait_for_initialized_storage(stop_receiver.0).await;
        tracing::info!("Verifier storage initialization precondition completed");
        result
    }
}
