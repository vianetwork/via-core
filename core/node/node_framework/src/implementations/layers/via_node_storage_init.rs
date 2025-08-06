use via_node_storage_init::ViaMainNodeStorageInitializer;
use zksync_config::configs::via_consensus::ViaGenesisConfig;

use crate::{
    implementations::resources::{
        pools::{MasterPool, PoolResource},
        via_btc_client::BtcClientResource,
        via_indexer_wallet::ViaSystemWalletsResource,
    },
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

#[derive(Debug)]
pub struct ViaNodeStorageInitializerLayer {
    via_genesis_config: ViaGenesisConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<MasterPool>,
    pub btc_client_resource: BtcClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub system_wallets_resource: ViaSystemWalletsResource,
}

impl ViaNodeStorageInitializerLayer {
    pub fn new(via_genesis_config: ViaGenesisConfig) -> Self {
        Self { via_genesis_config }
    }
}

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

        let initializer =
            ViaMainNodeStorageInitializer::new(pool, client.clone(), self.via_genesis_config);
        let system_wallets = initializer.indexer_wallets().await?;
        let system_wallets_resource = ViaSystemWalletsResource::from(system_wallets);

        Ok(Output {
            system_wallets_resource,
        })
    }
}
