use std::collections::VecDeque;

use via_btc_client::inscriber::test_utils::{MockBitcoinOps, MockBitcoinOpsConfig};
use zksync_config::configs::via_btc_sender::ViaGasAdjusterConfig;

use super::{GasStatisticsInner, ViaGasAdjuster};

/// Check that we compute the median correctly
#[test]
fn median() {
    let mut stats: GasStatisticsInner<u64> = GasStatisticsInner::new(5);
    stats.set_last_processed_block(5);
    stats.add_samples([6, 4, 7, 8, 4]);
    assert_eq!(stats.median(), 6);

    let mut stats: GasStatisticsInner<u64> = GasStatisticsInner::new(5);
    stats.set_last_processed_block(4);
    stats.add_samples([8, 4, 4, 10]);
    assert_eq!(stats.median(), 8);
}

/// Check that we properly manage the block base fee queue
#[test]
fn samples_queue() {
    let mut stats: GasStatisticsInner<u64> = GasStatisticsInner::new(5);
    stats.set_last_processed_block(5);
    stats.add_samples([6, 4, 7, 8, 4, 5]);
    assert_eq!(stats.samples, VecDeque::from([4, 7, 8, 4, 5]));
    stats.add_samples([18, 18, 18]);
    assert_eq!(stats.samples, VecDeque::from([4, 5, 18, 18, 18]));
}

#[tokio::test]
async fn test_bound_gas_price() {
    let gas_adjuster_config = ViaGasAdjusterConfig::for_tests();
    let client_mock = Box::new(MockBitcoinOps::new(MockBitcoinOpsConfig::default()));
    let gas_adjuster_mock = ViaGasAdjuster::new(client_mock, gas_adjuster_config.clone());

    // When gas price is bigger than the config max price, should return max price
    let mut current_gas_price: u64 = 1000;
    assert_eq!(
        gas_adjuster_config.max_l1_gas_price(),
        gas_adjuster_mock.bound_gas_price(current_gas_price)
    );

    // When gas price is smaller than the config max price, should return gas price
    current_gas_price = 50;
    assert_eq!(
        current_gas_price,
        gas_adjuster_mock.bound_gas_price(current_gas_price)
    );
}

#[tokio::test]
async fn test_estimate_effective_gas_price() {
    let gas_adjuster_config = ViaGasAdjusterConfig::for_tests();
    let fee_history: Vec<u64> = vec![10, 5, 15];

    let gas_stats: GasStatisticsInner<u64> = GasStatisticsInner::new(5);
    let config = MockBitcoinOpsConfig {
        fee_history,
        ..Default::default()
    };

    let client_mock = Box::new(MockBitcoinOps::new(config));
    let gas_adjuster_mock = ViaGasAdjuster::new(client_mock, gas_adjuster_config.clone());

    assert_eq!(
        gas_stats.median(),
        gas_adjuster_mock.estimate_effective_gas_price()
    );
}

#[tokio::test]
async fn test_estimate_effective_pubdata_price() {
    let gas_adjuster_config = ViaGasAdjusterConfig::for_tests();
    let client_mock = Box::new(MockBitcoinOps::new(MockBitcoinOpsConfig::default()));
    let gas_adjuster_mock = ViaGasAdjuster::new(client_mock, gas_adjuster_config.clone());
    assert_eq!(gas_adjuster_mock.estimate_effective_pubdata_price(), 0);
}

#[tokio::test]
async fn test_keep_updated() {
    let fee_history: Vec<u64> = vec![10, 5, 15];
    let block_height: u128 = 10;
    let gas_adjuster_config = ViaGasAdjusterConfig::for_tests();

    let mut config = MockBitcoinOpsConfig::default();
    config.set_block_height(block_height);
    config.set_fee_history(fee_history.clone());

    let client_mock = Box::new(MockBitcoinOps::new(config.clone()));
    let mut gas_adjuster_mock = ViaGasAdjuster::new(client_mock, gas_adjuster_config.clone());

    gas_adjuster_mock
        .base_fee_statistics
        .add_samples(fee_history.clone());
    gas_adjuster_mock
        .base_fee_statistics
        .set_last_processed_block(block_height as usize);

    assert_eq!(
        gas_adjuster_mock.base_fee_statistics.last_processed_block() as u128,
        block_height
    );

    assert_eq!(
        gas_adjuster_mock
            .base_fee_statistics
            .0
            .read()
            .unwrap()
            .samples,
        fee_history
    );

    gas_adjuster_mock.keep_updated().await.unwrap();

    let block_height: u128 = 13;
    let fee_history: Vec<u64> = vec![20, 10, 5];

    config.set_block_height(block_height);
    config.set_fee_history(fee_history.clone());

    let client_mock = Box::new(MockBitcoinOps::new(config));

    gas_adjuster_mock.set_client(client_mock);
    gas_adjuster_mock.keep_updated().await.unwrap();

    assert_eq!(
        gas_adjuster_mock.base_fee_statistics.last_processed_block() as u128,
        block_height
    );

    assert_eq!(
        gas_adjuster_mock
            .base_fee_statistics
            .0
            .read()
            .unwrap()
            .samples,
        fee_history
    );
}
