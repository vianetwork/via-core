use std::fmt::Debug;

use zksync_da_client::DataAvailabilityClient;
use zksync_node_framework::{
    implementations::resources::da_client::DAClientResource,
    service::ServiceContext,
    wiring_layer::{WiringError, WiringLayer},
};

use crate::CelestiaClient;

#[derive(Debug, Default)]
pub struct ViaNoDAClientWiringLayer;

#[async_trait::async_trait]
impl WiringLayer for ViaNoDAClientWiringLayer {
    fn layer_name(&self) -> &'static str {
        "via_no_da_layer"
    }

    async fn wire(self: Box<Self>, mut context: ServiceContext<'_>) -> Result<(), WiringError> {
        let client = CelestiaClient::new().await?;
        let client: Box<dyn DataAvailabilityClient> = Box::new(client);

        context.insert_resource(DAClientResource(client))?;

        Ok(())
    }
}
