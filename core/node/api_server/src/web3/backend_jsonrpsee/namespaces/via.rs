use bitcoin::Network;
use zksync_web3_decl::{
    jsonrpsee::core::{async_trait, RpcResult},
    namespaces::{DaBlobData, ViaNamespaceServer},
};

use crate::web3::namespaces::ViaNamespace;

#[async_trait]
impl ViaNamespaceServer for ViaNamespace {
    async fn get_bridge_address(&self) -> RpcResult<String> {
        self.get_bridge_address_impl()
            .await
            .map_err(|err| self.current_method().map_err(err))
    }

    async fn get_bitcoin_network(&self) -> RpcResult<Network> {
        Ok(self.get_bitcoin_network_impl())
    }

    async fn get_da_blob_data(&self, blob_id: String) -> RpcResult<Option<DaBlobData>> {
        self.get_da_blob_data_impl(blob_id)
            .await
            .map_err(|err| self.current_method().map_err(err))
    }
}
