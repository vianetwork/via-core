use bitcoin::Network;
use zksync_web3_decl::{
    jsonrpsee::core::{async_trait, RpcResult},
    namespaces::ViaNamespaceServer,
};

use crate::web3::namespaces::ViaNamespace;

#[async_trait]
impl ViaNamespaceServer for ViaNamespace {
    async fn get_bridge_address(&self) -> RpcResult<String> {
        Ok(self.get_bridge_address_impl())
    }

    async fn get_bitcoin_network(&self) -> RpcResult<Network> {
        Ok(self.get_bitcoin_network_impl())
    }
}
