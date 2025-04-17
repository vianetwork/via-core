use via_btc_client::inscriber::test_utils::{
    get_mock_inscriber_and_conditions, MockBitcoinOpsConfig,
};
use via_verifier_dal::{ConnectionPool, Verifier};
use zksync_config::ViaBtcSenderConfig;

use crate::btc_inscription_manager::ViaBtcInscriptionManager;

pub fn get_btc_sender_config(
    max_aggregated_blocks_to_commit: i32,
    max_aggregated_proofs_to_commit: i32,
) -> ViaBtcSenderConfig {
    let mut config = ViaBtcSenderConfig::for_tests();
    config.max_aggregated_blocks_to_commit = max_aggregated_blocks_to_commit;
    config.max_aggregated_proofs_to_commit = max_aggregated_proofs_to_commit;
    config
}

pub async fn get_inscription_manager_mock(
    pool: ConnectionPool<Verifier>,
    config: ViaBtcSenderConfig,
    mock_btc_ops_config: MockBitcoinOpsConfig,
) -> ViaBtcInscriptionManager {
    let inscriber = get_mock_inscriber_and_conditions(mock_btc_ops_config);
    Result::unwrap(ViaBtcInscriptionManager::new(inscriber, pool, config).await)
}
