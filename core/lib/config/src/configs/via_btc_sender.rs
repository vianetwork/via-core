use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ViaBtcSenderConfig {
    pub rpc_url: String,
    pub rpc_user: String,
    pub rpc_password: String,

    /// Network of the Bitcoin node.
    pub network: String,

    // SEQUENCER/ VERIFIER
    pub actor_role: String,
}

impl ViaBtcSenderConfig {
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    pub fn rpc_user(&self) -> &str {
        &self.rpc_user
    }

    pub fn rpc_password(&self) -> &str {
        &self.rpc_password
    }

    pub fn network(&self) -> &str {
        &self.network
    }

    // SEQUENCER/ VERIFIER
    pub fn actor_role(&self) -> &str {
        &self.actor_role
    }
}

impl ViaBtcSenderConfig {
    // Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> Self {
        Self {
            rpc_url: "http://localhost:18332".to_string(),
            rpc_user: "user".to_string(),
            rpc_password: "pass".to_string(),
            network: "regtest".to_string(),
            actor_role: "sequencer".to_string(),
        }
    }
}
