use std::{collections::HashMap, str::FromStr};

use anyhow::Context;
use bitcoin::Network;
use via_da_client::{
    pubdata::Pubdata,
    types::{L2BridgeLogMetadata, L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR},
};
use via_verifier_types::withdrawal::WithdrawalRequest;
use zksync_da_client::DataAvailabilityClient;
use zksync_types::{
    api::{BlockNumber, Log},
    web3::keccak256,
    Address, L1BatchNumber, H160, H256,
};
use zksync_web3_decl::{
    client::{DynClient, L2},
    namespaces::{EthNamespaceClient, ZksNamespaceClient},
    types::FilterBuilder,
};

use crate::withdraw::parse_l2_withdrawal_message;

#[derive(Debug, Clone)]
pub struct WithdrawalClient {
    pub network: Network,
    client: Box<dyn DataAvailabilityClient>,
    web3_client: Box<DynClient<L2>>,
}

impl WithdrawalClient {
    pub fn new(
        client: Box<dyn DataAvailabilityClient>,
        network: Network,
        web3_client: Box<DynClient<L2>>,
    ) -> Self {
        Self {
            client,
            network,
            web3_client,
        }
    }

    pub async fn get_withdrawals(
        &self,
        blob_id: &str,
        l1_block_number: L1BatchNumber,
    ) -> anyhow::Result<Vec<WithdrawalRequest>> {
        let pubdata_bytes = self
            .fetch_pubdata(blob_id)
            .await
            .with_context(|| "Failed to fetch pubdata from DA")?;
        let pubdata = Pubdata::decode_pubdata(pubdata_bytes)?;
        let l2_bridge_metadata = WithdrawalClient::list_l2_bridge_metadata(&pubdata);
        let logs = self.get_withdrawal_logs_from_node(l1_block_number).await?;
        let withdrawals =
            WithdrawalClient::get_valid_withdrawals(self.network, l2_bridge_metadata, logs)?;
        Ok(withdrawals)
    }

    async fn fetch_pubdata(&self, blob_id: &str) -> anyhow::Result<Vec<u8>> {
        let response = self.client.get_inclusion_data(blob_id).await?;
        if let Some(inclusion_data) = response {
            return Ok(inclusion_data.data);
        };
        Ok(Vec::new())
    }

    async fn get_withdrawal_logs_from_node(
        &self,
        l1_batch_number: L1BatchNumber,
    ) -> anyhow::Result<Vec<Log>> {
        let Some((start_block, end_block)) =
            self.web3_client.get_l2_block_range(l1_batch_number).await?
        else {
            tracing::warn!(
                "No batch range was found for the L1 batch {}",
                l1_batch_number.0,
            );
            return Ok(vec![]);
        };

        tracing::info!(
            "The L1 batch {} includes a range of blocks from {} to {}",
            l1_batch_number,
            start_block.as_u64(),
            end_block.as_u64()
        );
        let withdrawal_topic = H256::from_slice(&keccak256(b"Withdrawal(address,bytes,uint256)"));

        let filter = FilterBuilder::default()
            .set_from_block(BlockNumber::Number(start_block))
            .set_to_block(BlockNumber::Number(end_block))
            .set_address(vec![L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR
                .parse::<Address>()
                .unwrap()])
            .set_topics(Some(vec![withdrawal_topic]), None, None, None)
            .build();

        let logs = self.web3_client.get_logs(filter).await?;

        Ok(logs)
    }

    fn l2_to_l1_messages_hashmap(pubdata: &Pubdata) -> HashMap<H256, Vec<u8>> {
        let mut hashes: HashMap<H256, Vec<u8>> = HashMap::new();
        for message in &pubdata.l2_to_l1_messages {
            let hash = H256::from(keccak256(message));
            hashes.insert(hash, message.clone());
        }
        hashes
    }

    fn list_l2_bridge_metadata(pubdata: &Pubdata) -> Vec<L2BridgeLogMetadata> {
        let mut withdrawals: Vec<L2BridgeLogMetadata> = Vec::new();
        let l2_bridges_hash =
            H256::from(H160::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR).unwrap());
        let l2_to_l1_messages_hashmap = WithdrawalClient::l2_to_l1_messages_hashmap(pubdata);
        for log in pubdata.user_logs.clone() {
            // Ignore the logs if not from emitted from the L2 bridge contract
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

    fn get_valid_withdrawals(
        network: Network,
        l2_bridge_logs_metadata: Vec<L2BridgeLogMetadata>,
        logs: Vec<Log>,
    ) -> anyhow::Result<Vec<WithdrawalRequest>> {
        let mut withdrawal_requests: Vec<WithdrawalRequest> = Vec::new();
        if logs.len() != l2_bridge_logs_metadata.len() {
            tracing::info!("Skip the batch as the pubdata and the logs doesn't match");
            return Ok(vec![]);
        }

        for (i, l2_bridge_log_metadata) in l2_bridge_logs_metadata.iter().enumerate() {
            let log = logs[i].clone();

            let withdrawal_request =
                parse_l2_withdrawal_message(l2_bridge_log_metadata.message.clone(), log, network);

            if let Ok(req) = withdrawal_request {
                withdrawal_requests.push(req)
            }
        }
        Ok(withdrawal_requests)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::{Address, Amount};
    use ethers::abi::{encode, Token};
    use zksync_types::{web3::Bytes, U256, U64};

    use super::*;

    #[test]
    fn test_l2_to_l1_messages_hashmap() {
        let input = "00000001000100000000000000000000000000000000000000008008000000000000000000000000000000000000000000000000000000000000800aa1fd131a17718668a78581197d19972abd907b7b343b9694e02246d18c3801c500000001000000506c0960f962637274317178326c6b30756e756b6d3830716d65706a703439687766397a36786e7a307337336b396a35360000000000000000000000000000000000000000000000000000000005f5e10000000000010001280400032c1818e4770f08c05b28829d7d5f9d401d492c7432c166dfecf4af04238ea323009d7042e8fb0f249338d18505e5ba1d4a546e9d21f47c847ca725ff53ac29f740ca1bbc31cc849a8092a36f9a321e17412dee200b956038af1c2dc83430a0e8b000d3e2c6760d91078e517a2cb882cd3c9551de3ab5f30d554d51b17e3744cf92b0cf368ce957aed709b985423cd3ba11615de01ecafa15eb9a11bc6cdef4f6327900436ef22b96a07224eb06f0eecfecc184033da7db2a5fb58f867f17298b896b55000000420901000000362205f5e1000000003721032b8b14000000382209216c140000003a8901000000000000000000000000000000170000003b8902000000000000000000000000000000170000003e890200000000000000000000000000000017";
        let encoded_pubdata = hex::decode(input).unwrap();
        let pubdata = Pubdata::decode_pubdata(encoded_pubdata).unwrap();

        let hashes = WithdrawalClient::l2_to_l1_messages_hashmap(&pubdata);
        let hash = pubdata.user_logs[0].value;
        assert_eq!(hashes[&hash], pubdata.l2_to_l1_messages[0]);
    }

    #[test]
    fn test_list_l2_bridge_metadata() {
        let input = "00000001000100000000000000000000000000000000000000008008000000000000000000000000000000000000000000000000000000000000800aa1fd131a17718668a78581197d19972abd907b7b343b9694e02246d18c3801c500000001000000506c0960f962637274317178326c6b30756e756b6d3830716d65706a703439687766397a36786e7a307337336b396a35360000000000000000000000000000000000000000000000000000000005f5e10000000000010001280400032c1818e4770f08c05b28829d7d5f9d401d492c7432c166dfecf4af04238ea323009d7042e8fb0f249338d18505e5ba1d4a546e9d21f47c847ca725ff53ac29f740ca1bbc31cc849a8092a36f9a321e17412dee200b956038af1c2dc83430a0e8b000d3e2c6760d91078e517a2cb882cd3c9551de3ab5f30d554d51b17e3744cf92b0cf368ce957aed709b985423cd3ba11615de01ecafa15eb9a11bc6cdef4f6327900436ef22b96a07224eb06f0eecfecc184033da7db2a5fb58f867f17298b896b55000000420901000000362205f5e1000000003721032b8b14000000382209216c140000003a8901000000000000000000000000000000170000003b8902000000000000000000000000000000170000003e890200000000000000000000000000000017";
        let encoded_pubdata = hex::decode(input).unwrap();
        let pubdata = Pubdata::decode_pubdata(encoded_pubdata).unwrap();

        let hashes = WithdrawalClient::l2_to_l1_messages_hashmap(&pubdata);
        let hash = pubdata.user_logs[0].value;
        assert_eq!(hashes[&hash], pubdata.l2_to_l1_messages[0]);

        let l2_bridge_logs_metadata = WithdrawalClient::list_l2_bridge_metadata(&pubdata);
        assert_eq!(l2_bridge_logs_metadata.len(), 1);
        assert_eq!(
            l2_bridge_logs_metadata[0].message,
            pubdata.clone().l2_to_l1_messages[0]
        );
        assert_eq!(
            l2_bridge_logs_metadata[0].log.value,
            pubdata.user_logs[0].value
        );
    }

    #[test]
    fn test_get_valid_withdrawals() -> anyhow::Result<()> {
        let btc_bytes = b"bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56".to_vec();
        let amount = U256::from("0000000000000000000000000000000000000000000000000de0b6b3a7640000");
        let encoded_data = encode(&[Token::Bytes(btc_bytes.clone()), Token::Uint(amount.clone())]);
        let data = Bytes::from(encoded_data);

        let log = Log {
            block_timestamp: None,
            l1_batch_number: Some(U64::one()),
            address: H160::random(),
            topics: vec![
                H256::from_str(
                    "0x2d6ef0fc97a54b2a96a5f3c96e3e69dca5b8d5ef4f68f01472c9e7c2b8d1f17b",
                )
                .unwrap(),
                H256::from_str(
                    "0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .unwrap(),
            ],
            data,
            block_hash: None,
            block_number: Some(U64::one()),
            transaction_hash: Some(H256::zero()),
            transaction_index: None,
            log_index: Some(U256::zero()),
            transaction_log_index: Some(U256::zero()),
            log_type: None,
            removed: None,
        };

        let input = "00000001000100000000000000000000000000000000000000008008000000000000000000000000000000000000000000000000000000000000800aa1fd131a17718668a78581197d19972abd907b7b343b9694e02246d18c3801c500000001000000506c0960f962637274317178326c6b30756e756b6d3830716d65706a703439687766397a36786e7a307337336b396a35360000000000000000000000000000000000000000000000000000000005f5e10000000000010001280400032c1818e4770f08c05b28829d7d5f9d401d492c7432c166dfecf4af04238ea323009d7042e8fb0f249338d18505e5ba1d4a546e9d21f47c847ca725ff53ac29f740ca1bbc31cc849a8092a36f9a321e17412dee200b956038af1c2dc83430a0e8b000d3e2c6760d91078e517a2cb882cd3c9551de3ab5f30d554d51b17e3744cf92b0cf368ce957aed709b985423cd3ba11615de01ecafa15eb9a11bc6cdef4f6327900436ef22b96a07224eb06f0eecfecc184033da7db2a5fb58f867f17298b896b55000000420901000000362205f5e1000000003721032b8b14000000382209216c140000003a8901000000000000000000000000000000170000003b8902000000000000000000000000000000170000003e890200000000000000000000000000000017";
        let encoded_pubdata = hex::decode(input).unwrap();
        let pubdata = Pubdata::decode_pubdata(encoded_pubdata).unwrap();

        let hashes = WithdrawalClient::l2_to_l1_messages_hashmap(&pubdata);
        let hash = pubdata.user_logs[0].value;
        assert_eq!(hashes[&hash], pubdata.l2_to_l1_messages[0]);

        let l2_bridge_logs_metadata = WithdrawalClient::list_l2_bridge_metadata(&pubdata);
        let withdrawals = WithdrawalClient::get_valid_withdrawals(
            Network::Regtest,
            l2_bridge_logs_metadata,
            vec![log],
        )?;
        let expected_user_address =
            Address::from_str("bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56")
                .unwrap()
                .assume_checked();
        assert_eq!(withdrawals.len(), 1);
        assert_eq!(&withdrawals[0].receiver, &expected_user_address);
        let expected_amount = Amount::from_sat(100000000);
        assert_eq!(&withdrawals[0].amount, &expected_amount);

        Ok(())
    }
}
