use std::fmt::Debug;

use zksync_config::{
    configs::{via_celestia::DaBackend, via_secrets::ViaDASecrets},
    ViaCelestiaConfig,
};
use zksync_da_client::DataAvailabilityClient;
use zksync_node_framework::{
    implementations::resources::da_client::DAClientResource,
    wiring_layer::{WiringError, WiringLayer},
    IntoContext,
};
use zksync_types::url::SensitiveUrl;

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

#[derive(Debug, IntoContext)]
pub struct Output {
    pub client: DAClientResource,
}

#[async_trait::async_trait]
impl WiringLayer for ViaDaClientWiringLayer {
    type Input = ();
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_da_layer"
    }

    async fn wire(self, _input: Self::Input) -> Result<Self::Output, WiringError> {
        let primary_client: Box<dyn DataAvailabilityClient> = match self.config.da_backend {
            DaBackend::Celestia => Box::new(
                CelestiaClient::new(self.secrets.unwrap(), self.config.blob_size_limit).await?,
            ),
            DaBackend::Http => Box::new(HttpDaClient::new(self.config.api_node_url.clone())),
        };

        // If fallback external node URL is configured, wrap the primary client with a fallback client
        let client: Box<dyn DataAvailabilityClient> =
            if let Some(fallback_url) = &self.config.fallback_external_node_url {
                tracing::info!(
                    "Configuring DA client with fallback to external node at: {}",
                    fallback_url
                );
                let fallback_client = Box::new(ExternalNodeDaClient::new(
                    SensitiveUrl::from(fallback_url.parse().map_err(|e| {
                        WiringError::Configuration(format!("Invalid fallback URL: {}", e))
                    })?),
                )?);
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
