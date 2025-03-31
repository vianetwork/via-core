use bitcoin::Network;
#[cfg_attr(not(feature = "server"), allow(unused_imports))]
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;

use crate::client::{ForWeb3Network, L2};

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
}
