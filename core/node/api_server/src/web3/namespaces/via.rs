use bitcoin::Network;

use crate::web3::RpcState;

#[derive(Debug)]
pub(crate) struct ViaNamespace {
    state: RpcState,
}

impl ViaNamespace {
    pub fn new(state: RpcState) -> Self {
        Self { state }
    }

    pub fn get_bridge_address_impl(&self) -> String {
        self.state.api_config.via_bridge_address.clone()
    }

    pub fn get_bitcoin_network_impl(&self) -> Network {
        self.state.api_config.via_network
    }
}
