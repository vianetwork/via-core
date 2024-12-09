pub(crate) use l1_to_l2::L1ToL2MessageProcessor;
use via_btc_client::types::FullInscriptionMessage;
pub(crate) use votable::VotableMessageProcessor;
use zksync_dal::{Connection, Core};

mod l1_to_l2;
mod votable;

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
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
    ) -> Result<(), MessageProcessorError>;
}
