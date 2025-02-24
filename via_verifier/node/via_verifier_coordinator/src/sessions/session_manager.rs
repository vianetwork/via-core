use std::{collections::HashMap, sync::Arc};

use bitcoin::Txid;

use crate::{
    traits::ISession,
    types::{SessionOperation, SessionType},
};

pub struct SessionManager {
    pub sessions: HashMap<SessionType, Arc<dyn ISession>>,
}

impl SessionManager {
    pub fn new(sessions: HashMap<SessionType, Arc<dyn ISession>>) -> Self {
        Self { sessions }
    }

    pub async fn get_next_session(&self) -> anyhow::Result<Option<SessionOperation>> {
        for session in self.sessions.values() {
            if let Some(op) = session.session().await? {
                return Ok(Some(op));
            }
        }
        Ok(None)
    }

    pub async fn is_session_in_progress(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let session = self
            .sessions
            .get(&session_op.get_session_type())
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        session.verify_message(session_op).await
    }

    pub async fn verify_message(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        let session = self
            .sessions
            .get(&session_op.get_session_type())
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        session.verify_message(session_op).await
    }

    pub async fn before_process_session(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let session = self
            .sessions
            .get(&session_op.get_session_type())
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        session.before_process_session(session_op).await
    }

    pub async fn before_broadcast_final_transaction(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let session = self
            .sessions
            .get(&session_op.get_session_type())
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        session.before_broadcast_final_transaction(session_op).await
    }

    pub async fn after_broadcast_final_transaction(
        &self,
        txid: Txid,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let session = self
            .sessions
            .get(&session_op.get_session_type())
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

        session
            .after_broadcast_final_transaction(txid, session_op)
            .await
    }
}
