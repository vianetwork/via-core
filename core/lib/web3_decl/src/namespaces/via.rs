use bitcoin::Network;
#[cfg_attr(not(feature = "server"), allow(unused_imports))]
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;

use crate::client::{ForWeb3Network, L2};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DaBlobData {
    pub data: String, // hex-encoded blob data
}

#[cfg_attr(
    feature = "server",
    rpc(server, client, namespace = "via", client_bounds(Self: ForWeb3Network<Net = L2>))
)]
#[cfg_attr(
    not(feature = "server"),
    rpc(client, namespace = "via", client_bounds(Self: ForWeb3Network<Net = L2>))
)]
pub trait ViaNamespace {
    #[method(name = "getBridgeAddress")]
    async fn get_bridge_address(&self) -> RpcResult<String>;

    #[method(name = "getBitcoinNetwork")]
    async fn get_bitcoin_network(&self) -> RpcResult<Network>;

    /// Get DA blob data for a specific blob_id
    #[method(name = "getDaBlobData")]
    async fn get_da_blob_data(&self, blob_id: String) -> RpcResult<Option<DaBlobData>>;
}
