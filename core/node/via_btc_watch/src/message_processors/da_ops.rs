use via_btc_client::types::FullInscriptionMessage;
use zksync_dal::{Connection, Core};

use crate::message_processors::{MessageProcessor, MessageProcessorError};

#[derive(Debug)]
pub struct DAOpsMessageProcessor {}

impl DAOpsMessageProcessor {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl MessageProcessor for DAOpsMessageProcessor {
    async fn process_messages(
        &mut self,
        _storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
    ) -> Result<(), MessageProcessorError> {
        for msg in msgs {
            match msg {
                FullInscriptionMessage::L1BatchDAReference(_) => {}
                FullInscriptionMessage::ProofDAReference(_) => {}
                _ => continue,
            }
            tracing::debug!("Processing DA op: {:?}", msg);
        }
        Ok(())
    }
}
