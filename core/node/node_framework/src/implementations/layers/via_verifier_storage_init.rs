use via_verifier_storage_init::ViaVerifierStorageInitializer;
use zksync_config::{configs::via_consensus::ViaGenesisConfig, GenesisConfig, ViaBtcWatchConfig};

use crate::{
    implementations::resources::{
        pools::{PoolResource, VerifierPool},
        via_btc_client::BtcClientResource,
    },
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for via verifier initialization.
#[derive(Debug)]
pub struct ViaVerifierInitLayer {
    pub via_genesis_config: ViaGenesisConfig,
    pub via_btc_watch_config: ViaBtcWatchConfig,
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
pub struct Output {}

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

        ViaVerifierStorageInitializer::new(
            pool,
            client,
            self.via_genesis_config,
            self.genesis,
            self.via_btc_watch_config,
        )
        .await?;

        Ok(Output {})
    }
}
