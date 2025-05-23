use std::{path::PathBuf, time::Duration};

use serde::Deserialize;
use tokio::sync::Semaphore;
use zksync_contracts::test_contracts::LoadnextContractExecutionParams;
use zksync_types::L2ChainId;
use zksync_utils::workspace_dir_or_current_dir;

/// Configuration for the loadtest.
///
/// This structure is meant to provide the least possible amount of parameters:
/// By the ideology of the test, it is OK for it to be opinionated. Thus we don't provide
/// kinds of operations we want to perform, do not configure fail or pass criteria.
///
/// It is expected that the user will provide the basic settings, and the loadtest will
/// take care of everything else.
#[derive(Debug, Clone, Deserialize)]
pub struct LoadtestConfig {
    /// Address of the Bitcoin RPC.
    #[serde(default = "default_l1_btc_rpc_address")]
    pub l1_btc_rpc_address: String,

    /// Username of the Bitcoin RPC.
    #[serde(default = "default_l1_btc_rpc_username")]
    pub l1_btc_rpc_username: String,

    /// Password of the Bitcoin RPC.
    #[serde(default = "default_l1_btc_rpc_password")]
    pub l1_btc_rpc_password: String,

    /// Ethereum private key of the wallet that has funds to perform a test.
    #[serde(default = "default_eth_master_wallet_pk")]
    pub eth_master_wallet_pk: String,

    /// Bitcoin private key of the wallet that has funds to perform a test.
    #[serde(default = "default_btc_master_wallet_pk")]
    pub btc_master_wallet_pk: String,

    /// Amount of accounts to be used in test.
    /// This option configures the "width" of the test:
    /// how many concurrent operation flows will be executed.
    /// The higher the value is, the more load will be put on the node.
    /// If testing the sequencer throughput, this number must be sufficiently high.
    #[serde(default = "default_accounts_amount")]
    pub accounts_amount: usize,

    /// Duration of the test. For proper results, this value should be at least 10 minutes.
    #[serde(default = "default_duration_sec")]
    pub duration_sec: u64,

    /// Path to test contracts bytecode and ABI required for sending
    /// deploy and execute L2 transactions. Each folder in the path is expected
    /// to have the following structure:
    ///```ignore
    /// .
    /// ├── bytecode
    /// └── abi.json
    ///```
    /// Contract folder names names are not restricted.
    ///
    /// An example:
    ///```ignore
    /// .
    /// ├── erc-20
    /// │   ├── bytecode
    /// │   └── abi.json
    /// └── simple-contract
    ///     ├── bytecode
    ///     └── abi.json
    ///```
    #[serde(default = "default_test_contracts_path")]
    pub test_contracts_path: PathBuf,
    /// Limits the number of simultaneous API requests being performed at any moment of time.
    ///
    /// Setting it to:
    /// - 0 turns off API requests.
    /// - `accounts_amount` relieves the limit.
    #[serde(default = "default_sync_api_requests_limit")]
    pub sync_api_requests_limit: usize,

    /// Limits the number of simultaneously active PubSub subscriptions at any moment of time.
    ///
    /// Setting it to:
    /// - 0 turns off PubSub subscriptions.
    #[serde(default = "default_sync_pubsub_subscriptions_limit")]
    pub sync_pubsub_subscriptions_limit: usize,

    /// Time in seconds for a subscription to be active. Subscription will be closed after that time.
    #[serde(default = "default_single_subscription_time_secs")]
    pub single_subscription_time_secs: u64,

    /// Optional seed to be used in the test: normally you don't need to set the seed,
    /// but you can re-use seed from previous run to reproduce the sequence of operations locally.
    /// Seed must be represented as a hexadecimal string.
    ///
    /// Using the same seed doesn't guarantee reproducibility of API requests: unlike operations, these
    /// are generated in flight by multiple accounts in parallel.
    #[serde(default = "default_seed")]
    pub seed: Option<String>,

    /// Chain id of L2 node.
    #[serde(default = "default_l2_chain_id")]
    pub l2_chain_id: u64,

    /// RPC address of L2 node.
    #[serde(default = "default_l2_rpc_address")]
    pub l2_rpc_address: String,

    /// WS RPC address of L2 node.
    #[serde(default = "default_l2_ws_rpc_address")]
    pub l2_ws_rpc_address: String,

    /// The maximum number of transactions per account that can be sent without waiting for confirmation.
    /// Should not exceed the corresponding value in the L2 node configuration.
    #[serde(default = "default_max_inflight_txs")]
    pub max_inflight_txs: usize,

    /// All of test accounts get split into groups that share the
    /// deployed contract address. This helps to emulate the behavior of
    /// sending `Execute` to the same contract and reading its events by
    /// single a group. This value should be less than or equal to `ACCOUNTS_AMOUNT`.
    #[serde(default = "default_accounts_group_size")]
    pub accounts_group_size: usize,

    /// The expected number of the processed transactions during loadtest
    /// that should be compared to the actual result.
    /// If the value is `None`, the comparison is not performed.
    #[serde(default = "default_expected_tx_count")]
    pub expected_tx_count: Option<usize>,

    /// Label to use for results pushed to Prometheus.
    #[serde(default = "default_prometheus_label")]
    pub prometheus_label: String,

    /// Fail the load test immediately if a failure is encountered that would result
    /// in an eventual test failure anyway (e.g., a failure processing transactions).
    #[serde(default)]
    pub fail_fast: bool,

    /// use pay master to pay the transaction fee
    #[serde(default)]
    pub use_paymaster: bool,

    /// The via bridge address
    #[serde(default = "default_bridge_address")]
    pub bridge_address: String,
}

fn default_max_inflight_txs() -> usize {
    let result = 5;
    tracing::info!("Using default MAX_INFLIGHT_TXS: {result}");
    result
}

fn default_l1_btc_rpc_address() -> String {
    let result = "http://127.0.0.1:18443".to_string();
    tracing::info!("Using default L1_BTC_RPC_ADDRESS: {result}");
    result
}

fn default_l1_btc_rpc_username() -> String {
    let result = "rpcuser".to_string();
    tracing::info!("Using default L1_BTC_RPC_USERNAME: {result}");
    result
}

fn default_l1_btc_rpc_password() -> String {
    let result = "rpcpassword".to_string();
    tracing::info!("Using default L1_BTC_RPC_PASSWORD: {result}");
    result
}

fn default_eth_master_wallet_pk() -> String {
    // Use this key only for localhost because it is compromised!
    // Using this key for Testnet will result in losing Testnet ETH.
    // Corresponding wallet is `0x36615Cf349d7F6344891B1e7CA7C72883F5dc049`
    let result = "7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110".to_string();
    tracing::info!("Using default MASTER_WALLET_PK: {result}");
    result
}

// bcrt1q8tuqv885kehnzucdfskuw6mrhxcj7cjs4gfk5z
fn default_btc_master_wallet_pk() -> String {
    let result = "cVn486kDX5Mr9MimMyiRNMR4ZsKaLbLho3MHZgqVriB5q3S8FKKF".to_string();
    tracing::info!("Using default BTC_MASTER_WALLET_PK: {result}");
    result
}

fn default_accounts_amount() -> usize {
    let result = 10;
    tracing::info!("Using default ACCOUNTS_AMOUNT: {result}");
    result
}

fn default_duration_sec() -> u64 {
    let result = 60;
    tracing::info!("Using default DURATION_SEC: {result}");
    result
}

fn default_accounts_group_size() -> usize {
    let result = 1;
    tracing::info!("Using default ACCOUNTS_GROUP_SIZE: {result}");
    result
}

fn default_test_contracts_path() -> PathBuf {
    let test_contracts_path = workspace_dir_or_current_dir().join("etc/contracts-test-data");
    tracing::info!("Test contracts path: {}", test_contracts_path.display());
    test_contracts_path
}

fn default_sync_api_requests_limit() -> usize {
    let result = 20;
    tracing::info!("Using default SYNC_API_REQUESTS_LIMIT: {result}");
    result
}

fn default_sync_pubsub_subscriptions_limit() -> usize {
    let result = 150;
    tracing::info!("Using default SYNC_PUBSUB_SUBSCRIPTIONS_LIMIT: {result}");
    result
}

fn default_single_subscription_time_secs() -> u64 {
    let result = 30;
    tracing::info!("Using default SINGLE_SUBSCRIPTION_TIME_SECS: {result}");
    result
}

fn default_seed() -> Option<String> {
    let result = None;
    tracing::info!("Using default SEED: {result:?}");
    result
}

fn default_l2_chain_id() -> u64 {
    // 270 for Rinkeby
    let result = L2ChainId::default().as_u64();
    tracing::info!("Using default L2_CHAIN_ID: {result}");
    result
}

pub fn get_default_l2_rpc_address() -> String {
    "http://127.0.0.1:3050".to_string()
}

fn default_l2_rpc_address() -> String {
    // `https://z2-dev-api.zksync.dev:443` for stage2
    let result = get_default_l2_rpc_address();
    tracing::info!("Using default L2_RPC_ADDRESS: {result}");
    result
}

fn default_l2_ws_rpc_address() -> String {
    // `ws://z2-dev-api.zksync.dev:80/ws` for stage2
    let result = "ws://127.0.0.1:3051".to_string();
    tracing::info!("Using default L2_WS_RPC_ADDRESS: {result}");
    result
}

fn default_expected_tx_count() -> Option<usize> {
    let result = None;
    tracing::info!("Using default EXPECTED_TX_COUNT: {result:?}");
    result
}

fn default_prometheus_label() -> String {
    let result = "unset".to_string();
    tracing::info!("Using default PROMETHEUS_LABEL: {result:?}");
    result
}

fn default_bridge_address() -> String {
    let bridge_musig2_address =
        String::from("bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq");
    tracing::info!("Using default Bridge address: {bridge_musig2_address:?}");
    bridge_musig2_address
}

impl LoadtestConfig {
    pub fn from_env() -> envy::Result<Self> {
        envy::from_env()
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs(self.duration_sec)
    }
}

/// Configuration for the weights of loadtest operations
/// We use a random selection based on weight of operations. To perform some operations frequently, the developer must set the weight higher.
///
/// This configuration is independent from the main config for preserving simplicity of the main config
/// and do not break the backward compatibility
#[derive(Debug)]
pub struct ExecutionConfig {
    pub transaction_weights: TransactionWeights,
    pub contract_execution_params: LoadnextContractExecutionParams,
}

impl ExecutionConfig {
    pub fn from_env() -> Self {
        let transaction_weights =
            TransactionWeights::from_env().unwrap_or_else(default_transaction_weights);
        let contract_execution_params = LoadnextContractExecutionParams::from_env()
            .unwrap_or_else(default_contract_execution_params);
        Self {
            transaction_weights,
            contract_execution_params,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransactionWeights {
    pub deposit: f32,
    pub withdrawal: f32,
    pub l2_transactions: f32,
}

impl TransactionWeights {
    pub fn from_env() -> Option<Self> {
        envy::prefixed("TRANSACTION_WEIGHTS_").from_env().ok()
    }
}

impl Default for TransactionWeights {
    fn default() -> Self {
        Self {
            deposit: 0.05,
            withdrawal: 0.5,
            l2_transactions: 1.0,
        }
    }
}

fn default_transaction_weights() -> TransactionWeights {
    let result = TransactionWeights::default();
    tracing::info!("Using default TransactionWeights: {result:?}");
    result
}

fn default_contract_execution_params() -> LoadnextContractExecutionParams {
    let result = LoadnextContractExecutionParams::default();
    tracing::info!("Using default LoadnextContractExecutionParams: {result:?}");
    result
}

#[derive(Debug)]
pub struct RequestLimiters {
    pub api_requests: Semaphore,
    pub subscriptions: Semaphore,
}

impl RequestLimiters {
    pub fn new(config: &LoadtestConfig) -> Self {
        Self {
            api_requests: Semaphore::new(config.sync_api_requests_limit),
            subscriptions: Semaphore::new(config.sync_pubsub_subscriptions_limit),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::fs_utils::loadnext_contract;

    #[test]
    fn check_read_test_contract() {
        let test_contracts_path = default_test_contracts_path();
        loadnext_contract(&test_contracts_path).unwrap();
    }
}
