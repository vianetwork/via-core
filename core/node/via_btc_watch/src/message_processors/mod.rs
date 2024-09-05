pub(crate) use da_ops::DAOpsMessageProcessor;
pub(crate) use l1_to_l2::L1ToL2MessageProcessor;
pub(crate) use system_ops::SystemOpsMessageProcessor;
use via_btc_client::types::FullInscriptionMessage;
use zksync_dal::{Connection, Core};

mod da_ops;
mod l1_to_l2;
mod system_ops;

#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub(super) enum MessageProcessorError {
    #[error("failed parsing a log into {msg_kind}: {source:?}")]
    MsgParse {
        msg_kind: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("internal processing error: {0:?}")]
    Internal(#[from] anyhow::Error),
}

#[allow(dead_code)]
impl MessageProcessorError {
    pub fn log_parse(source: impl Into<anyhow::Error>, msg_kind: &'static str) -> Self {
        Self::MsgParse {
            msg_kind,
            source: source.into(),
        }
    }
}

#[async_trait::async_trait]
pub(super) trait MessageProcessor: 'static + std::fmt::Debug + Send + Sync {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
    ) -> Result<(), MessageProcessorError>;
}
