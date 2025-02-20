use via_btc_client::inscriber::test_utils::{
    get_mock_inscriber_and_conditions, MockBitcoinOpsConfig,
};
use via_verifier_dal::{ConnectionPool, Verifier};
use zksync_config::{configs::via_btc_sender::ProofSendingMode, ViaBtcSenderConfig};

use crate::btc_inscription_manager::ViaBtcInscriptionManager;

pub fn get_btc_sender_config(
    max_aggregated_blocks_to_commit: i32,
    max_aggregated_proofs_to_commit: i32,
) -> ViaBtcSenderConfig {
    ViaBtcSenderConfig {
        actor_role: "sender".to_string(),
        network: "testnet".to_string(),
        private_key: "0x0".to_string(),
        rpc_password: "password".to_string(),
        rpc_url: "password".to_string(),
        rpc_user: "rpc".to_string(),
        poll_interval: 5000,
        da_identifier: "CELESTIA".to_string(),
        max_aggregated_blocks_to_commit,
        max_aggregated_proofs_to_commit,
        max_txs_in_flight: 1,
        proof_sending_mode: ProofSendingMode::SkipEveryProof,
        block_confirmations: 0,
    }
}

pub async fn get_inscription_manager_mock(
    pool: ConnectionPool<Verifier>,
    config: ViaBtcSenderConfig,
    mock_btc_ops_config: MockBitcoinOpsConfig,
) -> ViaBtcInscriptionManager {
    let inscriber = get_mock_inscriber_and_conditions(mock_btc_ops_config);
    Result::unwrap(ViaBtcInscriptionManager::new(inscriber, pool, config).await)
}
