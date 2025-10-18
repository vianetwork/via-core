use anyhow::anyhow;
use bitcoin::Network;
use zksync_dal::{CoreDal, DalError};
use zksync_types::via_wallet::SystemWallets;
use zksync_web3_decl::{error::Web3Error, namespaces::DaBlobData};

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

    pub async fn get_da_blob_data_impl(
        &self,
        blob_id: String,
    ) -> Result<Option<DaBlobData>, Web3Error> {
        let mut conn = self
            .state
            .connection_pool
            .connection()
            .await
            .map_err(DalError::generalize)?;

        let Some(is_proof) = conn
            .via_data_availability_dal()
            .get_blob_type(&blob_id)
            .await
            .map_err(DalError::generalize)?
        else {
            return Ok(None);
        };

        let mut blob_data = DaBlobData {
            is_proof,
            ..Default::default()
        };

        if is_proof {
            let Some((mut prove_batch, _)) = conn
                .via_data_availability_dal()
                .get_proof_data_by_blob_id(&blob_id)
                .await
                .map_err(DalError::generalize)?
            else {
                return Ok(None);
            };

            prove_batch.should_verify = self.state.api_config.via_dispatch_real_proof;

            let proof_data = match bincode::serialize(&prove_batch) {
                Ok(data) => data,
                Err(e) => {
                    return Err(Web3Error::InternalError(anyhow::anyhow!(
                        "Error serializing prove_batch: {e}"
                    )));
                }
            };

            blob_data.proof_data = hex::encode(proof_data);
        } else {
            let Some((_, pub_data)) = conn
                .via_data_availability_dal()
                .get_da_blob_pub_data_by_blob_id(&blob_id)
                .await
                .map_err(DalError::generalize)?
            else {
                return Ok(None);
            };
            blob_data.pub_data = hex::encode(pub_data);
        }

        Ok(Some(blob_data))
    }
}
