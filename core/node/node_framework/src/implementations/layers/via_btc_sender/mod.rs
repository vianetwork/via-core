pub mod aggregator;
pub mod manager;
pub mod vote;
pub mod vote_manager;

use via_btc_client::inscriber::InscriberPolicy;
use zksync_config::ViaBtcSenderConfig;

pub(crate) fn inscriber_policy_from_config(config: &ViaBtcSenderConfig) -> anyhow::Result<InscriberPolicy> {
    InscriberPolicy::from_sats(
        config.min_inscription_output_sats(),
        config.min_change_output_sats(),
        config.allow_unconfirmed_change_reuse(),
        config.min_feerate_sat_vb(),
        config.min_feerate_chained_sat_vb(),
        config.max_feerate_sat_vb(),
        config.escalation_step_sat_vb(),
    )
}
