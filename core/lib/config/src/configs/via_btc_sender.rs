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

    /// Minimum inscription output value to stay comfortably above dust-like policy floors.
    pub min_inscription_output_sats: Option<u64>,

    /// Minimum reusable change output value.
    pub min_change_output_sats: Option<u64>,

    /// Whether unconfirmed reveal-change outputs from the in-memory inscriber context may be reused.
    pub allow_unconfirmed_change_reuse: Option<bool>,

    /// Minimum feerate for normal inscription construction.
    pub min_feerate_sat_vb: Option<u64>,

    /// Minimum feerate when the sender is already operating on a pending chain.
    pub min_feerate_chained_sat_vb: Option<u64>,

    /// Maximum feerate cap to avoid runaway overpay behavior.
    pub max_feerate_sat_vb: Option<u64>,

    /// Additional sat/vB step applied as pending chain depth grows.
    pub escalation_step_sat_vb: Option<u64>,

    /// Minimum age before attempting a replacement / re-broadcast for a stuck inscription.
    pub escalation_interval_sec: Option<u64>,

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

    pub fn allow_unconfirmed_change_reuse(&self) -> bool {
        self.allow_unconfirmed_change_reuse.unwrap_or(false)
    }

    pub fn min_feerate_sat_vb(&self) -> u64 {
        self.min_feerate_sat_vb.unwrap_or(8)
    }

    pub fn min_feerate_chained_sat_vb(&self) -> u64 {
        self.min_feerate_chained_sat_vb.unwrap_or(20)
    }

    pub fn max_feerate_sat_vb(&self) -> u64 {
        self.max_feerate_sat_vb.unwrap_or(80)
    }

    pub fn escalation_step_sat_vb(&self) -> u64 {
        self.escalation_step_sat_vb.unwrap_or(5)
    }

    pub fn escalation_interval_sec(&self) -> u64 {
        self.escalation_interval_sec.unwrap_or(900)
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
            allow_unconfirmed_change_reuse: None,
            min_feerate_sat_vb: None,
            min_feerate_chained_sat_vb: None,
            max_feerate_sat_vb: None,
            escalation_step_sat_vb: None,
            escalation_interval_sec: None,
            max_pending_chain_depth: None,
            min_spendable_balance_sats: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ViaBtcSenderConfig;

    #[test]
    fn test_guardrail_defaults() {
        let config = ViaBtcSenderConfig::for_tests();

        assert_eq!(config.max_pending_chain_depth(), 3);
        assert_eq!(config.min_spendable_balance_sats(), 2_000);
    }
}
