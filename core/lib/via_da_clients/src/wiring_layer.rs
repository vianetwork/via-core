use std::fmt::Debug;

use zksync_config::{
    configs::{via_celestia::DaBackend, via_secrets::ViaDASecrets},
    ViaCelestiaConfig,
};
use zksync_da_client::DataAvailabilityClient;
use zksync_node_framework::{
    implementations::resources::{da_client::DAClientResource, eth_interface::L2InterfaceResource},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

use crate::{
    celestia::client::CelestiaClient, external_node::client::ExternalNodeDaClient,
    fallback::client::FallbackDaClient, http::client::HttpDaClient,
};

#[derive(Debug)]
pub struct ViaDaClientWiringLayer {
    config: ViaCelestiaConfig,
    secrets: Option<ViaDASecrets>,
}

impl ViaDaClientWiringLayer {
    pub fn new(config: ViaCelestiaConfig, secrets: Option<ViaDASecrets>) -> Self {
        Self { config, secrets }
    }
}

#[derive(Debug, FromContext)]
#[context(crate = zksync_node_framework)]
pub struct Input {
    /// L2 query client resource, provided by ViaQueryEthClientLayer
    /// Only required when fallback to external node is configured
    pub query_client_l2: Option<L2InterfaceResource>,
}

#[derive(Debug, IntoContext)]
#[context(crate = zksync_node_framework)]
pub struct Output {
    pub client: DAClientResource,
}

#[async_trait::async_trait]
impl WiringLayer for ViaDaClientWiringLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_da_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let primary_client: Box<dyn DataAvailabilityClient> = match self.config.da_backend {
            DaBackend::Celestia => Box::new(
                CelestiaClient::new(self.secrets.unwrap(), self.config.blob_size_limit).await?,
            ),
            DaBackend::Http => Box::new(HttpDaClient::new(self.config.api_node_url.clone())),
        };

        // If fallback external node URL is configured, wrap the primary client with a fallback client
        let client: Box<dyn DataAvailabilityClient> =
            if self.config.fallback_external_node_url.is_some() {
                tracing::info!(
                    "Configuring DA client with fallback to external node (using QueryClient)"
                );

                let mut fallback_client = None;
                if let Some(query_client_l2) = input.query_client_l2 {
                    // Use the QueryClient from the node framework instead of creating a new HTTP client
                    fallback_client = Some(Box::new(ExternalNodeDaClient::new(query_client_l2.0))
                        as Box<dyn DataAvailabilityClient>);
                }

                Box::new(FallbackDaClient::new(
                    primary_client,
                    fallback_client,
                    self.config.verify_consistency,
                ))
            } else {
                tracing::info!("DA client configured without fallback");
                primary_client
            };

        Ok(Output {
            client: DAClientResource(client),
        })
    }
}
