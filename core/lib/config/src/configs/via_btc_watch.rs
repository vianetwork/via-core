use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Copy)]
pub enum ActorRole {
    Sequencer,
    Verifier,
}

/// Total L1 blocks to process at a time.
pub const L1_BLOCKS_CHUNK: u32 = 10;

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

    /// The starting L1 block number from which indexing begins
    pub start_l1_block_number: u32,

    /// When set to true, the btc_watch starts indexing L1 blocks from the "start_l1_block_number".
    pub restart_indexing: bool,

    /// The agreement threshold required for the verifier to finalize an L1 batch.
    pub zk_agreement_threshold: f64,
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

    /// Returns the starting L1 block number from which indexing begins.
    pub fn start_l1_block_number(&self) -> u32 {
        self.start_l1_block_number
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
            start_l1_block_number: 1,
            restart_indexing: false,
            zk_agreement_threshold: 0.5,
        }
    }
}
