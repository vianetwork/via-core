use via_node_storage_init::ViaMainNodeStorageInitializer;
use zksync_config::{configs::via_consensus::ViaGenesisConfig, ViaBtcWatchConfig};

use crate::{
    implementations::resources::{
        pools::{MasterPool, PoolResource},
        via_btc_client::BtcClientResource,
    },
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

#[derive(Debug)]
pub struct ViaNodeStorageInitializerLayer {
    pub via_genesis_config: ViaGenesisConfig,
    pub via_btc_watch_config: ViaBtcWatchConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<MasterPool>,
    pub btc_client_resource: BtcClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {}

#[async_trait::async_trait]
impl WiringLayer for ViaNodeStorageInitializerLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "Via_node_storage_initializer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let client = input.btc_client_resource.default;
        let pool = input.master_pool.get().await?;

        ViaMainNodeStorageInitializer::new(
            pool,
            client.clone(),
            self.via_genesis_config,
            self.via_btc_watch_config,
        )
        .await?;

        Ok(Output {})
    }
}
