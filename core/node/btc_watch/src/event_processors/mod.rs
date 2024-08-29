use std::fmt;

use zksync_dal::{Connection, Core};

#[derive(Debug, thiserror::Error)]
pub(super) enum EventProcessorError {
    #[error("failed parsing a log into {log_kind}: {source:?}")]
    LogParse {
        log_kind: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("internal processing error: {0:?}")]
    Internal(#[from] anyhow::Error),
}

impl EventProcessorError {
    pub fn log_parse(source: impl Into<anyhow::Error>, log_kind: &'static str) -> Self {
        Self::LogParse {
            log_kind,
            source: source.into(),
        }
    }
}

#[async_trait::async_trait]
pub(super) trait EventProcessor: 'static + fmt::Debug + Send + Sync {
    async fn process_events(
        &mut self,
        storage: &mut Connection<'_, Core>,
    ) -> Result<(), EventProcessorError>;
}
