pub(crate) use l1_to_l2::L1ToL2MessageProcessor;
use via_btc_client::types::FullInscriptionMessage;
use zksync_dal::{Connection, Core};
use zksync_types::PriorityOpId;

mod l1_to_l2;

#[derive(Debug, thiserror::Error)]
pub(super) enum MessageProcessorError {
    #[error("internal processing error: {0:?}")]
    Internal(#[from] anyhow::Error),
    #[error("gap detected in priority operations")]
    PriorityOpsGap,
    #[error("priority id mismatch: expected {expected}, got {actual}")]
    PriorityIdMismatch {
        expected: PriorityOpId,
        actual: PriorityOpId,
    },
    #[error("database error: {0}")]
    DatabaseError(String),
    #[error("ethereum address parsing error")]
    EthAddressParsingError,
}

#[async_trait::async_trait]
pub(super) trait MessageProcessor: 'static + std::fmt::Debug + Send + Sync {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
    ) -> Result<(), MessageProcessorError>;
}
