pub(crate) use governance_upgrade::GovernanceUpgradesEventProcessor;
pub(crate) use l1_to_l2::L1ToL2MessageProcessor;
pub(crate) use system_wallet::SystemWalletProcessor;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinTxid, FullInscriptionMessage},
};
pub(crate) use votable::VotableMessageProcessor;
use zksync_dal::{Connection, Core, DalError};
use zksync_types::H256;

mod governance_upgrade;
mod l1_to_l2;
mod system_wallet;
mod votable;

#[derive(Debug, thiserror::Error)]
pub(super) enum MessageProcessorError {
    #[error("internal processing error: {0:?}")]
    Internal(#[from] anyhow::Error),
    #[error("database error: {0}")]
    DatabaseError(String),
}

impl From<DalError> for MessageProcessorError {
    fn from(err: DalError) -> Self {
        MessageProcessorError::DatabaseError(err.to_string())
    }
}

#[async_trait::async_trait]
pub(super) trait MessageProcessor: 'static + std::fmt::Debug + Send + Sync {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError>;
}

pub(crate) fn convert_txid_to_h256(txid: BitcoinTxid) -> H256 {
    let mut tx_id_bytes = txid.as_raw_hash()[..].to_vec();
    tx_id_bytes.reverse();
    H256::from_slice(&tx_id_bytes)
}
