use via_btc_client::indexer::BitcoinInscriptionIndexer;
use zksync_config::configs::{via_btc_client::ViaBtcClientConfig, via_consensus::ViaGenesisConfig};

use crate::{
    implementations::resources::{
        via_btc_client::BtcClientResource, via_btc_indexer::BtcIndexerResource,
    },
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

#[derive(Debug)]
pub struct ViaIndexerLayer {
    via_genesis_config: ViaGenesisConfig,
    via_btc_client: ViaBtcClientConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub btc_client_resource: BtcClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub btc_indexer_resource: BtcIndexerResource,
}

impl ViaIndexerLayer {
    pub fn new(via_genesis_config: ViaGenesisConfig, via_btc_client: ViaBtcClientConfig) -> Self {
        Self {
            via_genesis_config,
            via_btc_client,
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaIndexerLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_indexer_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let client = input.btc_client_resource.default;
        let indexer = BitcoinInscriptionIndexer::new(
            client,
            self.via_btc_client.clone(),
            self.via_genesis_config.bootstrap_txids()?,
        )
        .await
        .map_err(|e| WiringError::Internal(e.into()))?;
        let btc_indexer_resource = BtcIndexerResource::from(indexer.clone());

        Ok(Output {
            btc_indexer_resource,
        })
    }
}
