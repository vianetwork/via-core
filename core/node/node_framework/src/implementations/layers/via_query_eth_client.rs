use anyhow::Context;
use zksync_types::url::SensitiveUrl;
use zksync_web3_decl::client::Client;

use crate::{
    implementations::resources::eth_interface::L2InterfaceResource,
    wiring_layer::{WiringError, WiringLayer},
    IntoContext,
};

/// Wiring layer for Ethereum client.
#[derive(Debug)]
pub struct ViaQueryEthClientLayer {
    web3_url: SensitiveUrl,
}

impl ViaQueryEthClientLayer {
    pub fn new(web3_url: SensitiveUrl) -> Self {
        Self { web3_url }
    }
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    query_client_l2: L2InterfaceResource,
}

#[async_trait::async_trait]
impl WiringLayer for ViaQueryEthClientLayer {
    type Input = ();
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_query_eth_client_layer"
    }

    async fn wire(self, _input: Self::Input) -> Result<Output, WiringError> {
        let query_client_l2 = L2InterfaceResource(Box::new(
            Client::http(self.web3_url.clone())
                .context("Client::new()")?
                .build(),
        ));

        Ok(Output { query_client_l2 })
    }
}
