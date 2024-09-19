use bitcoin::Txid;
use zksync_basic_types::{L1BatchNumber, H256};

#[derive(Clone)]
pub struct ViaBtcL1BlockDetails {
    pub number: L1BatchNumber,
    pub timestamp: i64,
    pub hash: H256,
    pub commit_tx_id: Txid,
    pub reveal_tx_id: Txid,
    pub blob_id: String,
}

impl std::fmt::Debug for ViaBtcL1BlockDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViaBtcBlockDetails")
            .field("number", &self.number)
            .field("timestamp", &self.timestamp)
            .field("blob_id", &self.blob_id)
            .field("commit_tx_id", &self.commit_tx_id)
            .field("reveal_tx_id", &self.reveal_tx_id)
            .finish()
    }
}
