use std::time::Duration;

use serde::{Deserialize, Serialize};

const DEFAULT_DA_LAYER: &str = "celestia";

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ViaBtcSenderConfig {
    /// Service interval in milliseconds.
    pub poll_interval: u64,

    // Number of blocks to commit at time, should be 'one'.
    pub max_aggregated_blocks_to_commit: i32,

    // Number of proofs to commit at time, should be 'one'.
    pub max_aggregated_proofs_to_commit: i32,

    // The max number of inscription in flight
    pub max_txs_in_flight: i64,

    /// Number of block confirmations required to mark the inscription request as confirmed.
    pub block_confirmations: u32,

    /// The identifier of the DA layer.
    pub da_identifier: Option<String>,

    /// The btc sender wallet address.
    pub wallet_address: String,

    /// The number of blocks to wait before considering an inscription stuck.
    pub stuck_inscription_block_number: Option<u32>,

    /// The required time (seconds) to wait before create a commit inscription.
    pub block_time_to_commit: Option<u32>,

    /// The required time (seconds) to wait before create a proof inscription.
    pub block_time_to_proof: Option<u32>,

    /// Hard floor for taproot inscription output to avoid dust / borderline outputs.
    pub min_inscription_output_sats: Option<u64>,

    /// Hard floor for change output to keep spendable UTXOs healthy.
    pub min_change_output_sats: Option<u64>,

    /// Minimum fee-rate for non-chained transactions (sat/vB).
    pub min_feerate_sat_vb: Option<u64>,

    /// Minimum fee-rate when pending inscriptions exist in context (sat/vB).
    pub min_chained_feerate_sat_vb: Option<u64>,

    /// Maximum fee-rate safety cap (sat/vB).
    pub max_feerate_sat_vb: Option<u64>,

    /// Max number of pending inscriptions in context before pausing new sends.
    pub max_pending_chain_depth: Option<u32>,

    /// Do not send new inscriptions when trusted (confirmed) balance goes below this threshold.
    pub min_spendable_balance_sats: Option<u64>,
}

impl ViaBtcSenderConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval)
    }

    pub fn da_identifier(&self) -> String {
        self.da_identifier
            .as_ref()
            .unwrap_or(&String::from(DEFAULT_DA_LAYER))
            .clone()
    }

    pub fn block_time_to_commit(&self) -> u32 {
        self.block_time_to_commit.unwrap_or_default()
    }

    pub fn block_time_to_proof(&self) -> u32 {
        self.block_time_to_proof.unwrap_or_default()
    }

    pub fn stuck_inscription_block_number(&self) -> u32 {
        self.stuck_inscription_block_number.unwrap_or(6)
    }

    pub fn min_inscription_output_sats(&self) -> u64 {
        self.min_inscription_output_sats.unwrap_or(600)
    }

    pub fn min_change_output_sats(&self) -> u64 {
        self.min_change_output_sats.unwrap_or(1_000)
    }

    pub fn min_feerate_sat_vb(&self) -> u64 {
        self.min_feerate_sat_vb.unwrap_or(8)
    }

    pub fn min_chained_feerate_sat_vb(&self) -> u64 {
        self.min_chained_feerate_sat_vb.unwrap_or(20)
    }

    pub fn max_feerate_sat_vb(&self) -> u64 {
        self.max_feerate_sat_vb.unwrap_or(80)
    }

    pub fn max_pending_chain_depth(&self) -> u32 {
        self.max_pending_chain_depth.unwrap_or(3)
    }

    pub fn min_spendable_balance_sats(&self) -> u64 {
        self.min_spendable_balance_sats.unwrap_or(2_000)
    }
}

impl ViaBtcSenderConfig {
    // Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> Self {
        Self {
            poll_interval: 1000,
            max_aggregated_blocks_to_commit: 1,
            max_aggregated_proofs_to_commit: 1,
            max_txs_in_flight: 1,
            block_confirmations: 0,
            da_identifier: None,
            wallet_address: "".into(),
            block_time_to_commit: None,
            block_time_to_proof: None,
            stuck_inscription_block_number: None,
            min_inscription_output_sats: None,
            min_change_output_sats: None,
            min_feerate_sat_vb: None,
            min_chained_feerate_sat_vb: None,
            max_feerate_sat_vb: None,
            max_pending_chain_depth: None,
            min_spendable_balance_sats: None,
        }
    }
}
