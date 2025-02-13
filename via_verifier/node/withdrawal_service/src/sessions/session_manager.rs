use std::collections::HashMap;

use bitcoin::Txid;

use crate::{
    traits::ISession,
    types::{SessionOperation, SessionType},
};

pub struct SessionManager {
    pub sessions: HashMap<SessionType, Box<dyn ISession>>,
}

impl SessionManager {
    pub fn new(sessions: HashMap<SessionType, Box<dyn ISession>>) -> Self {
        Self { sessions }
    }

    pub async fn get_next_session(&self) -> anyhow::Result<Option<SessionOperation>> {
        for session in self.sessions.values() {
            let session_op = session.session().await?;
            if session_op.is_some() {
                return Ok(session_op);
            }
        }
        Ok(None)
    }

    pub async fn is_session_in_progress(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        Ok(match self.sessions.get(&session_op.get_session_type()) {
            Some(s) => s.is_session_in_progress(session_op).await?,
            None => return Ok(false),
        })
    }

    pub async fn verify_message(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        Ok(match self.sessions.get(&session_op.get_session_type()) {
            Some(s) => s.verify_message(session_op).await?,
            None => return Ok(false),
        })
    }

    pub async fn pre_process_session(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        Ok(match self.sessions.get(&session_op.get_session_type()) {
            Some(s) => s.pre_process_session(session_op).await?,
            None => return Ok(false),
        })
    }

    pub async fn before_broadcast_final_transaction(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        Ok(match self.sessions.get(&session_op.get_session_type()) {
            Some(s) => s.before_broadcast_final_transaction(session_op).await?,
            None => return Ok(false),
        })
    }

    pub async fn after_broadcast_final_transaction(
        &self,
        txid: Txid,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        Ok(match self.sessions.get(&session_op.get_session_type()) {
            Some(s) => {
                s.after_broadcast_final_transaction(txid, session_op)
                    .await?
            }
            None => return Ok(false),
        })
    }
}
