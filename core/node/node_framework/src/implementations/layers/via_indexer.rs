use via_btc_client::indexer::BitcoinInscriptionIndexer;

use crate::{
    implementations::resources::{
        via_btc_client::BtcClientResource, via_btc_indexer::BtcIndexerResource,
        via_indexer_wallet::ViaSystemWalletsResource,
    },
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

#[derive(Debug)]
pub struct ViaIndexerLayer {}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub btc_client_resource: BtcClientResource,
    pub system_wallets_resource: ViaSystemWalletsResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    pub btc_indexer_resource: BtcIndexerResource,
}

impl ViaIndexerLayer {
    pub fn new() -> Self {
        Self {}
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
        let system_wallets = input.system_wallets_resource.0;
        let indexer = BitcoinInscriptionIndexer::new(client, system_wallets);

        let btc_indexer_resource = BtcIndexerResource::from(indexer.clone());

        Ok(Output {
            btc_indexer_resource,
        })
    }
}
