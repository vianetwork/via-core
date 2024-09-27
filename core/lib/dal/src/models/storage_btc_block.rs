use std::str::FromStr;

use bitcoin::{hashes::Hash, Txid};
use zksync_types::{btc_block::ViaBtcL1BlockDetails, L1BatchNumber};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ViaBtcStorageL1BlockDetails {
    pub number: i64,
    pub timestamp: i64,
    pub hash: Option<Vec<u8>>,
    pub commit_tx_id: Option<String>,
    pub reveal_tx_id: Option<String>,
    pub blob_id: String,
}

impl From<ViaBtcStorageL1BlockDetails> for ViaBtcL1BlockDetails {
    fn from(details: ViaBtcStorageL1BlockDetails) -> Self {
        ViaBtcL1BlockDetails {
            number: L1BatchNumber::from(details.number as u32),
            timestamp: details.timestamp,
            hash: details.hash,
            commit_tx_id: Txid::from_str(&details.commit_tx_id.clone().unwrap_or_default())
                .unwrap_or(Txid::all_zeros()),
            reveal_tx_id: Txid::from_str(&details.commit_tx_id.clone().unwrap_or_default())
                .unwrap_or(Txid::all_zeros()),
            blob_id: details.blob_id,
        }
    }
}
