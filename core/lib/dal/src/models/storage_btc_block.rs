use bitcoin::{hashes::Hash, Txid};
use sqlx::types::chrono::{DateTime, NaiveDateTime, Utc};
use zksync_types::{
    api::{BlockDetails, BlockDetailsBase, BlockStatus, L1BatchDetails},
    btc_block::ViaBtcL1BlockDetails,
    via_utils::reverse_vec_to_h256,
    Address, L1BatchNumber, L2BlockNumber, H256,
};

use super::storage_block::convert_base_system_contracts_hashes;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ViaBtcStorageL1BlockDetails {
    pub number: i64,
    pub timestamp: i64,
    pub hash: Option<Vec<u8>>,
    pub commit_tx_id: Option<Vec<u8>>,
    pub reveal_tx_id: Option<Vec<u8>>,
    pub blob_id: Option<String>,
    pub prev_l1_batch_hash: Option<Vec<u8>>,
}

impl From<ViaBtcStorageL1BlockDetails> for ViaBtcL1BlockDetails {
    fn from(details: ViaBtcStorageL1BlockDetails) -> Self {
        ViaBtcL1BlockDetails {
            number: L1BatchNumber::from(details.number as u32),
            timestamp: details.timestamp,
            hash: details.hash,
            commit_tx_id: Txid::from_slice(&details.commit_tx_id.clone().unwrap_or_default())
                .unwrap_or(Txid::all_zeros()),
            reveal_tx_id: Txid::from_slice(&details.reveal_tx_id.clone().unwrap_or_default())
                .unwrap_or(Txid::all_zeros()),
            blob_id: details.blob_id.unwrap_or_default(),
            prev_l1_batch_hash: details.prev_l1_batch_hash,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ViaStorageL1BatchDetails {
    pub number: i64,
    pub timestamp: i64,
    pub l1_tx_count: i32,
    pub l2_tx_count: i32,
    pub root_hash: Option<Vec<u8>>,
    pub commit_tx_hash: Option<Vec<u8>>,
    pub committed_at: Option<NaiveDateTime>,
    pub prove_tx_hash: Option<Vec<u8>>,
    pub proven_at: Option<NaiveDateTime>,
    pub is_finalized: Option<bool>,
    pub executed_at: Option<NaiveDateTime>,
    pub l1_gas_price: i64,
    pub l2_fair_gas_price: i64,
    pub fair_pubdata_price: Option<i64>,
    pub bootloader_code_hash: Option<Vec<u8>>,
    pub default_aa_code_hash: Option<Vec<u8>>,
    pub evm_emulator_code_hash: Option<Vec<u8>>,
}

impl From<ViaStorageL1BatchDetails> for L1BatchDetails {
    fn from(details: ViaStorageL1BatchDetails) -> Self {
        let status = if details.number == 0 || details.is_finalized.is_some() {
            BlockStatus::Verified
        } else {
            BlockStatus::Sealed
        };

        let base = BlockDetailsBase {
            timestamp: details.timestamp as u64,
            l1_tx_count: details.l1_tx_count as usize,
            l2_tx_count: details.l2_tx_count as usize,
            status,
            root_hash: details.root_hash.as_deref().map(H256::from_slice),
            commit_tx_hash: details.commit_tx_hash.map(|hash| reverse_vec_to_h256(hash)),
            committed_at: details
                .committed_at
                .map(|committed_at| DateTime::<Utc>::from_naive_utc_and_offset(committed_at, Utc)),
            commit_chain_id: None,
            prove_tx_hash: details.prove_tx_hash.map(|hash| reverse_vec_to_h256(hash)),
            proven_at: details
                .proven_at
                .map(|proven_at| DateTime::<Utc>::from_naive_utc_and_offset(proven_at, Utc)),
            prove_chain_id: None,
            execute_tx_hash: calculate_execution_hash(details.is_finalized),
            executed_at: details
                .executed_at
                .map(|executed_at| DateTime::<Utc>::from_naive_utc_and_offset(executed_at, Utc)),
            execute_chain_id: None,
            l1_gas_price: details.l1_gas_price as u64,
            l2_fair_gas_price: details.l2_fair_gas_price as u64,
            fair_pubdata_price: details.fair_pubdata_price.map(|x| x as u64),
            base_system_contracts_hashes: convert_base_system_contracts_hashes(
                details.bootloader_code_hash,
                details.default_aa_code_hash,
                details.evm_emulator_code_hash,
            ),
        };
        L1BatchDetails {
            base,
            number: L1BatchNumber(details.number as u32),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ViaStorageBlockDetails {
    pub number: i64,
    pub l1_batch_number: i64,
    pub timestamp: i64,
    pub l1_tx_count: i32,
    pub l2_tx_count: i32,
    pub root_hash: Option<Vec<u8>>,
    pub commit_tx_hash: Option<Vec<u8>>,
    pub committed_at: Option<NaiveDateTime>,
    pub prove_tx_hash: Option<Vec<u8>>,
    pub proven_at: Option<NaiveDateTime>,
    pub is_finalized: Option<bool>,
    pub executed_at: Option<NaiveDateTime>,
    // L1 gas price assumed in the corresponding batch
    pub l1_gas_price: i64,
    // L2 gas price assumed in the corresponding batch
    pub l2_fair_gas_price: i64,
    // Cost of publishing 1 byte (in wei).
    pub fair_pubdata_price: Option<i64>,
    pub bootloader_code_hash: Option<Vec<u8>>,
    pub default_aa_code_hash: Option<Vec<u8>>,
    pub evm_emulator_code_hash: Option<Vec<u8>>,
    pub fee_account_address: Vec<u8>,
    pub protocol_version: Option<i32>,
}

pub fn calculate_execution_hash(is_finalized: Option<bool>) -> Option<H256> {
    if is_finalized.is_some() {
        return Some(H256::repeat_byte(0x11));
    }
    Some(H256::zero())
}

impl From<ViaStorageBlockDetails> for BlockDetails {
    fn from(details: ViaStorageBlockDetails) -> Self {
        let status = if details.number == 0 || details.is_finalized.is_some() {
            BlockStatus::Verified
        } else {
            BlockStatus::Sealed
        };

        let base = BlockDetailsBase {
            timestamp: details.timestamp as u64,
            l1_tx_count: details.l1_tx_count as usize,
            l2_tx_count: details.l2_tx_count as usize,
            status,
            root_hash: details.root_hash.as_deref().map(H256::from_slice),
            commit_tx_hash: details.commit_tx_hash.map(|hash| reverse_vec_to_h256(hash)),
            committed_at: details
                .committed_at
                .map(|committed_at| DateTime::from_naive_utc_and_offset(committed_at, Utc)),
            commit_chain_id: None,
            prove_tx_hash: details.prove_tx_hash.map(|hash| reverse_vec_to_h256(hash)),
            proven_at: details
                .proven_at
                .map(|proven_at| DateTime::<Utc>::from_naive_utc_and_offset(proven_at, Utc)),
            prove_chain_id: None,
            execute_tx_hash: calculate_execution_hash(details.is_finalized),
            executed_at: details
                .executed_at
                .map(|executed_at| DateTime::<Utc>::from_naive_utc_and_offset(executed_at, Utc)),
            execute_chain_id: None,
            l1_gas_price: details.l1_gas_price as u64,
            l2_fair_gas_price: details.l2_fair_gas_price as u64,
            fair_pubdata_price: details.fair_pubdata_price.map(|x| x as u64),
            base_system_contracts_hashes: convert_base_system_contracts_hashes(
                details.bootloader_code_hash,
                details.default_aa_code_hash,
                details.evm_emulator_code_hash,
            ),
        };

        BlockDetails {
            base,
            number: L2BlockNumber(details.number as u32),
            l1_batch_number: L1BatchNumber(details.l1_batch_number as u32),
            operator_address: Address::from_slice(&details.fee_account_address),
            protocol_version: details
                .protocol_version
                .map(|v| (v as u16).try_into().unwrap()),
        }
    }
}
