use std::str::FromStr;

use anyhow::Context;
use bitcoin::Txid;
use zksync_types::btc_block::ViaBtcBlockDetails;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ViaBtcStorageBlockDetails {
    pub number: i64,
    pub hash: Option<Vec<u8>>,
    pub commit_tx_id: String,
    pub reveal_tx_id: String,
}

impl From<ViaBtcStorageBlockDetails> for ViaBtcBlockDetails {
    fn from(details: ViaBtcStorageBlockDetails) -> Self {
        ViaBtcBlockDetails {
            number: details.number,
            hash: details.hash,
            commit_tx_id: Txid::from_str(&details.commit_tx_id)
                .context("Failed to parse txid")
                .unwrap(),
            reveal_tx_id: Txid::from_str(&details.reveal_tx_id)
                .context("Failed to parse txid")
                .unwrap(),
        }
    }
}
