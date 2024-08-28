use std::fmt::Debug;

use zksync_config::ViaCelestiaConfig;
use zksync_da_client::DataAvailabilityClient;
use zksync_node_framework::{
    implementations::resources::da_client::DAClientResource,
    wiring_layer::{WiringError, WiringLayer},
    IntoContext,
};

use crate::celestia::client::CelestiaClient;

#[derive(Debug)]
pub struct ViaCelestiaClientWiringLayer {
    config: ViaCelestiaConfig,
}

impl ViaCelestiaClientWiringLayer {
    pub fn new(config: ViaCelestiaConfig) -> Self {
        Self { config }
    }
}

#[derive(Debug, IntoContext)]
pub struct Output {
    pub client: DAClientResource,
}

#[async_trait::async_trait]
impl WiringLayer for ViaCelestiaClientWiringLayer {
    type Input = ();
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_da_layer"
    }

    async fn wire(self, _input: Self::Input) -> Result<Self::Output, WiringError> {
        let client = CelestiaClient::new(self.config).await?;
        let client: Box<dyn DataAvailabilityClient> = Box::new(client);

        Ok(Output {
            client: DAClientResource(client),
        })
    }
}
