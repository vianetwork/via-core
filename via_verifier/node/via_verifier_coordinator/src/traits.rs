use std::any::Any;

use axum::async_trait;
use bitcoin::Txid;

use crate::types::SessionOperation;

#[async_trait]
pub trait ISession: Any + Send + Sync {
    async fn session(&self) -> anyhow::Result<Option<SessionOperation>>;

    async fn is_session_in_progress(
        &self,
        session_op_pts: &SessionOperation,
    ) -> anyhow::Result<bool>;

    async fn verify_message(&self, session_op: &SessionOperation) -> anyhow::Result<bool>;

    async fn before_process_session(&self, session_op: &SessionOperation) -> anyhow::Result<bool>;

    async fn before_broadcast_final_transaction(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool>;

    async fn after_broadcast_final_transaction(
        &self,
        txid: Txid,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool>;

    async fn is_bridge_session_already_processed(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool>;

    fn as_any(&self) -> &dyn Any;
}
