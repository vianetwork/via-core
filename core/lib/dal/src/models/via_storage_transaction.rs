use sqlx::types::chrono::{DateTime, NaiveDateTime, Utc};
use zksync_types::{
    api::{TransactionDetails, TransactionStatus},
    bigdecimal_to_u256,
    via_utils::reverse_vec_to_h256,
    H160, U256,
};

use super::storage_btc_block::calculate_execution_hash;
use crate::BigDecimal;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ViaStorageTransactionDetails {
    pub is_priority: bool,
    pub initiator_address: Vec<u8>,
    pub gas_limit: Option<BigDecimal>,
    pub gas_per_pubdata_limit: Option<BigDecimal>,
    pub received_at: NaiveDateTime,
    pub miniblock_number: Option<i64>,
    pub error: Option<String>,
    pub effective_gas_price: Option<BigDecimal>,
    pub refunded_gas: i64,
    pub commit_tx_hash: Option<Vec<u8>>,
    pub prove_tx_hash: Option<Vec<u8>>,
    pub is_finalized: Option<bool>,
}

impl ViaStorageTransactionDetails {
    fn get_transaction_status(&self) -> TransactionStatus {
        if self.error.is_some() {
            TransactionStatus::Failed
        } else if self.is_finalized.is_some() {
            TransactionStatus::Verified
        } else if self.miniblock_number.is_some() {
            TransactionStatus::Included
        } else {
            TransactionStatus::Pending
        }
    }
}

impl From<ViaStorageTransactionDetails> for TransactionDetails {
    fn from(tx_details: ViaStorageTransactionDetails) -> Self {
        let status = tx_details.get_transaction_status();

        let effective_gas_price =
            bigdecimal_to_u256(tx_details.effective_gas_price.unwrap_or_default());

        let gas_limit = bigdecimal_to_u256(
            tx_details
                .gas_limit
                .expect("gas limit is mandatory for transaction"),
        );
        let gas_refunded = U256::from(tx_details.refunded_gas as u64);
        let fee = (gas_limit - gas_refunded) * effective_gas_price;

        let gas_per_pubdata =
            bigdecimal_to_u256(tx_details.gas_per_pubdata_limit.unwrap_or_default());

        let initiator_address = H160::from_slice(tx_details.initiator_address.as_slice());
        let received_at = DateTime::<Utc>::from_naive_utc_and_offset(tx_details.received_at, Utc);

        let commit_tx_hash = tx_details
            .commit_tx_hash
            .map(|hash| reverse_vec_to_h256(hash));
        let prove_tx_hash = tx_details
            .prove_tx_hash
            .map(|hash| reverse_vec_to_h256(hash));
        let execute_tx_hash = calculate_execution_hash(tx_details.is_finalized);

        TransactionDetails {
            is_l1_originated: tx_details.is_priority,
            status,
            fee,
            gas_per_pubdata,
            initiator_address,
            received_at,
            commit_tx_hash,
            prove_tx_hash,
            execute_tx_hash,
        }
    }
}
