pub(crate) use verifier::VerifierMessageProcessor;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinTxid, FullInscriptionMessage},
};
use via_verifier_dal::{Connection, Verifier};
use zksync_types::H256;

mod verifier;

#[derive(Debug, thiserror::Error)]
pub(super) enum MessageProcessorError {
    #[error("internal processing error: {0:?}")]
    Internal(#[from] anyhow::Error),
    #[error("database error: {0}")]
    DatabaseError(String),
}

#[async_trait::async_trait]
pub(super) trait MessageProcessor: 'static + std::fmt::Debug + Send + Sync {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<(), MessageProcessorError>;
}

pub(crate) fn convert_txid_to_h256(txid: BitcoinTxid) -> H256 {
    let mut tx_id_bytes = txid.as_raw_hash()[..].to_vec();
    tx_id_bytes.reverse();
    H256::from_slice(&tx_id_bytes)
}
