use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub enum ActorRole {
    Sequencer,
    Verifier,
}

/// Configuration for the Bitcoin watch crate.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ViaBtcWatchConfig {
    /// How often we want to poll the Bitcoin node.
    /// Value in milliseconds.
    pub btc_node_poll_interval: u64,

    /// Amount of confirmations for the Bitcoin message to be processed.
    pub confirmations_for_btc_msg: Option<u64>,

    /// URL of the Bitcoin node RPC.
    pub rpc_url: String,

    /// Username for the Bitcoin node RPC.
    pub rpc_user: String,

    /// Password for the Bitcoin node RPC.
    pub rpc_password: String,

    /// Network of the Bitcoin node.
    pub network: String,

    /// List of transaction IDs to bootstrap the indexer.
    pub bootstrap_txids: Vec<String>,

    /// Role of the actor. SEQUENCER or VERIFIER.
    pub actor_role: ActorRole,

    /// Number of blocks that we should wait before processing the new blocks.
    pub btc_blocks_lag: u32,
}

impl ViaBtcWatchConfig {
    /// Converts `self.btc_node_poll_interval` into `Duration`.
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.btc_node_poll_interval)
    }

    /// Returns the amount of confirmations for the Bitcoin message to be processed.
    pub fn confirmations_for_btc_msg(&self) -> Option<u64> {
        self.confirmations_for_btc_msg
    }

    /// Returns the role of the actor.
    pub fn actor_role(&self) -> &ActorRole {
        &self.actor_role
    }

    /// Returns the number of blocks that we should wait before processing the new blocks.
    pub fn btc_blocks_lag(&self) -> u32 {
        self.btc_blocks_lag
    }

    /// Returns the RPC URL of the Bitcoin node.
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Returns the RPC user of the Bitcoin node.
    pub fn rpc_user(&self) -> &str {
        &self.rpc_user
    }

    /// Returns the RPC password of the Bitcoin node.
    pub fn rpc_password(&self) -> &str {
        &self.rpc_password
    }

    /// Returns the network of the Bitcoin node.
    pub fn network(&self) -> &str {
        &self.network
    }

    /// Returns the list of transaction IDs to bootstrap the indexer.
    pub fn bootstrap_txids(&self) -> Vec<String> {
        self.bootstrap_txids.clone()
    }
}

impl ViaBtcWatchConfig {
    /// Creates a mock configuration object suitable for unit tests.
    /// Values inside match the config used for localhost development.
    pub fn for_tests() -> Self {
        Self {
            btc_node_poll_interval: 1000,
            confirmations_for_btc_msg: Some(3),
            rpc_url: "http://localhost:18332".to_string(),
            rpc_user: "".to_string(),
            rpc_password: "".to_string(),
            network: "regtest".to_string(),
            bootstrap_txids: vec![],
            actor_role: ActorRole::Sequencer,
            btc_blocks_lag: 1,
        }
    }
}
