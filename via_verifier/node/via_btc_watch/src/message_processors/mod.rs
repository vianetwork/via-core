pub(crate) use governance_upgrade::GovernanceUpgradesEventProcessor;
pub(crate) use l1_to_l2::L1ToL2MessageProcessor;
pub(crate) use system_wallet::SystemWalletProcessor;
pub(crate) use verifier::VerifierMessageProcessor;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinTxid, FullInscriptionMessage, IndexerError},
};
use via_verifier_dal::{Connection, DalError, Verifier};
pub(crate) use withdrawal::WithdrawalProcessor;
use zksync_types::H256;

mod governance_upgrade;
mod l1_to_l2;
mod system_wallet;
mod verifier;
mod withdrawal;

#[derive(Debug, Clone, thiserror::Error)]
#[error("proof continuity gap: expected batch {expected_batch} after {canonical_parent:?}, observed batch {observed_batch} with parent {observed_parent:?}, proof {proof_txid:?} at Bitcoin {source_height}/{source_hash}; ingestion paused, reconcile canonical history before restarting")]
pub(super) struct ProofContinuityGap {
    pub expected_batch: u32,
    pub canonical_parent: Option<H256>,
    pub observed_batch: u32,
    pub observed_parent: H256,
    pub proof_txid: H256,
    pub source_height: u32,
    pub source_hash: bitcoin::BlockHash,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum MessageProcessorError {
    #[error(transparent)]
    ProofContinuityGap(Box<ProofContinuityGap>),
    #[error("internal processing error: {0:?}")]
    Internal(#[from] anyhow::Error),
    #[error("database error: {0}")]
    DatabaseError(String),
}

impl From<IndexerError> for MessageProcessorError {
    fn from(err: IndexerError) -> Self {
        MessageProcessorError::Internal(err.into())
    }
}

impl From<DalError> for MessageProcessorError {
    fn from(err: DalError) -> Self {
        MessageProcessorError::DatabaseError(err.to_string())
    }
}

#[async_trait::async_trait]
pub(super) trait MessageProcessor: 'static + std::fmt::Debug + Send + Sync {
    async fn process_messages(
        &mut self, storage: &mut Connection<'_, Verifier>, msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError>;
}

pub(crate) fn convert_txid_to_h256(txid: BitcoinTxid) -> H256 {
    let mut tx_id_bytes = txid.as_raw_hash()[..].to_vec();
    tx_id_bytes.reverse();
    H256::from_slice(&tx_id_bytes)
}
