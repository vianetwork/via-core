use std::{collections::HashMap, str::FromStr};

use zksync_da_client::DataAvailabilityClient;
use zksync_types::{web3::keccak256, H160, H256};

use crate::{
    pubdata::Pubdata,
    types::{L2BridgeLogMetadata, WithdrawalRequest, L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR},
    withdraw::parse_l2_withdrawal_message,
};

#[derive(Debug)]
pub struct WithdrawalClient {
    client: Box<dyn DataAvailabilityClient>,
}

impl WithdrawalClient {
    pub async fn get_withdrawals(&self, blob_id: &str) -> anyhow::Result<Vec<WithdrawalRequest>> {
        let pubdata_bytes = self._fetch_pubdata(blob_id).await?;
        let pubdata = Pubdata::decode_pubdata(pubdata_bytes)?;
        let l2_bridge_metadata = self._list_l2_bridge_metadata(pubdata);
        let withdrawals = self._get_valid_withdrawals(l2_bridge_metadata);
        Ok(withdrawals)
    }

    async fn _fetch_pubdata(&self, blob_id: &str) -> anyhow::Result<Vec<u8>> {
        let response = self.client.get_inclusion_data(blob_id).await?;
        if let Some(inclusion_data) = response {
            return Ok(inclusion_data.data);
        };
        Ok(Vec::new())
    }

    fn _l2_to_l1_messages_hashmap(&self, pubdata: &Pubdata) -> HashMap<H256, Vec<u8>> {
        let mut hashes: HashMap<H256, Vec<u8>> = HashMap::new();
        for message in &pubdata.l2_to_l1_messages {
            let hash = H256::from(keccak256(hex::encode(&message).as_bytes()));
            hashes.insert(hash, message.clone());
        }
        hashes
    }

    fn _list_l2_bridge_metadata(&self, pubdata: Pubdata) -> Vec<L2BridgeLogMetadata> {
        let mut withdrawals: Vec<L2BridgeLogMetadata> = Vec::new();
        let l2_bridges_hash =
            H256::from(H160::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR).unwrap());
        let l2_to_l1_messages_hashmap = self._l2_to_l1_messages_hashmap(&pubdata);
        for log in pubdata.user_logs.clone() {
            // Ignore the logs if not from emitted from the L2 bridge contract
            if log.key != l2_bridges_hash {
                continue;
            };

            if log.key != l2_bridges_hash {
                continue;
            };
            withdrawals.push(L2BridgeLogMetadata {
                message: l2_to_l1_messages_hashmap[&log.value].clone(),
                log,
            });
        }
        withdrawals
    }

    fn _get_valid_withdrawals(
        &self,
        l2_bridge_logs_metadata: Vec<L2BridgeLogMetadata>,
    ) -> Vec<WithdrawalRequest> {
        let mut withdrawal_requests: Vec<WithdrawalRequest> = Vec::new();
        for l2_bridge_log_metadata in l2_bridge_logs_metadata {
            let withdrawal_request = parse_l2_withdrawal_message(l2_bridge_log_metadata.message);

            match withdrawal_request {
                Ok(req) => withdrawal_requests.push(req),
                Err(_) => (),
            }
        }
        withdrawal_requests
    }
}
