use anyhow::anyhow;
use bitcoin::Network;
use zksync_dal::{CoreDal, DalError};
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

    pub(crate) fn current_method(&self) -> &MethodTracer {
        &self.state.current_method
    }

    pub async fn get_bridge_address_impl(&self) -> Result<String, Web3Error> {
        if let Some(system_wallets_raw) = self
            .state
            .connection_pool
            .connection()
            .await
            .map_err(DalError::generalize)?
            .via_wallet_dal()
            .get_system_wallets_raw(i64::MAX)
            .await
            .map_err(DalError::generalize)?
        {
            let system_wallets = SystemWallets::try_from(system_wallets_raw)?;
            return Ok(system_wallets.bridge.to_string());
        }

        Err(Web3Error::InternalError(anyhow!(
            "Bridge address not found"
        )))
    }

    pub fn get_bitcoin_network_impl(&self) -> Network {
        self.state.api_config.via_network
    }
}
