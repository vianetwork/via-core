use via_btc_client::inscriber::InscriberPolicy;
use zksync_config::ViaBtcSenderConfig;

pub(super) fn build_inscriber_policy(config: &ViaBtcSenderConfig) -> InscriberPolicy {
    InscriberPolicy {
        min_inscription_output_sats: config.min_inscription_output_sats(),
        min_change_output_sats: config.min_change_output_sats(),
        min_feerate_sat_vb: config.min_feerate_sat_vb(),
        min_chained_feerate_sat_vb: config.min_chained_feerate_sat_vb(),
        max_feerate_sat_vb: config.max_feerate_sat_vb(),
    }
}
