use bitcoin::Network;
use zksync_dal::CoreDal;
use zksync_types::via_wallet::SystemWallets;
use zksync_web3_decl::error::Web3Error;

use crate::web3::{backend_jsonrpsee::MethodTracer, RpcState};

#[derive(Debug)]
pub(crate) struct ViaNamespace {
    state: RpcState,
}

impl ViaNamespace {
    pub fn new(state: RpcState) -> Self {
        Self { state }
    }

    pub fn current_method(&self) -> &MethodTracer {
        &self.state.current_method
    }

    pub async fn get_bridge_address_impl(&self) -> anyhow::Result<String> {
        let Some(wallets_map) = self
            .state
            .tx_sender
            .0
            .replica_connection_pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await?
        else {
            return Err(
                Web3Error::InternalError(anyhow::anyhow!("Bridge address not found")).into(),
            );
        };

        let wallets = SystemWallets::try_from(wallets_map)?;
        Ok(wallets.bridge.to_string())
    }

    pub fn get_bitcoin_network_impl(&self) -> Network {
        self.state.api_config.via_network
    }
}
