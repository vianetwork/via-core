use zksync_config::{configs::via_consensus::ViaGenesisConfig};

use crate::{
    implementations::resources::{
        pools::{MasterPool, PoolResource},
        via_btc_client::BtcClientResource,
    },
    wiring_layer::{WiringError, WiringLayer},
    FromContext,
};

#[derive(Debug)]
pub struct ViaNodeStorageInitializerLayer {
    pub via_genesis_config: ViaGenesisConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<MasterPool>,
    pub btc_client_resource: BtcClientResource,
}

#[async_trait::async_trait]
impl WiringLayer for ViaNodeStorageInitializerLayer {
    type Input = Input;
    type Output = ();

    fn layer_name(&self) -> &'static str {
        "Via_node_storage_initializer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let client = input.btc_client_resource.default;
        let pool = input.master_pool.get().await?;

        via_btc_storage::initialize_reader(&pool, client, self.via_genesis_config).await?;

        Ok(())
    }
}
