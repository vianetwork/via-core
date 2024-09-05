use via_btc_client::types::FullInscriptionMessage;
use zksync_dal::{Connection, Core};

use crate::message_processors::{MessageProcessor, MessageProcessorError};

#[derive(Debug)]
pub struct SystemOpsMessageProcessor {}

impl SystemOpsMessageProcessor {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl MessageProcessor for SystemOpsMessageProcessor {
    async fn process_messages(
        &mut self,
        _storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
    ) -> Result<(), MessageProcessorError> {
        for msg in msgs {
            match msg {
                FullInscriptionMessage::ValidatorAttestation(_) => {}
                FullInscriptionMessage::ProposeSequencer(_) => {}
                FullInscriptionMessage::SystemBootstrapping(_) => {
                    // not possible, as this message is handled by the main loop at the beginning
                }
                _ => continue,
            }
            tracing::debug!("Processing system op: {:?}", msg);
        }
        Ok(())
    }
}
