#![allow(dead_code)]
use std::fmt;

use zksync_dal::{Connection, Core};

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

impl MessageProcessorError {
    pub fn log_parse(source: impl Into<anyhow::Error>, msg_kind: &'static str) -> Self {
        Self::MsgParse {
            msg_kind,
            source: source.into(),
        }
    }
}

#[async_trait::async_trait]
pub(super) trait MessageProcessor: 'static + fmt::Debug + Send + Sync {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> Result<(), MessageProcessorError>;
}
