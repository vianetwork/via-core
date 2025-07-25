//! Testing harness for the IO.

use std::{slice, sync::Arc, time::Duration};

use via_btc_client::{
    client::BitcoinClient,
    inscriber::test_utils::{get_mock_inscriber_and_conditions, MockBitcoinOpsConfig},
};
use via_fee_model::{ViaGasAdjuster, ViaMainNodeFeeInputProvider};
use zksync_config::{
    configs::{chain::StateKeeperConfig, via_btc_client::ViaBtcClientConfig, wallets::Wallets},
    GasAdjusterConfig,
};
use zksync_contracts::BaseSystemContracts;
use zksync_dal::{ConnectionPool, Core, CoreDal};
use zksync_multivm::{
    interface::{TransactionExecutionMetrics, TransactionExecutionResult},
    vm_latest::constants::BATCH_COMPUTATIONAL_GAS_LIMIT,
};
use zksync_node_genesis::create_genesis_l1_batch;
use zksync_node_test_utils::{
    create_l1_batch, create_l2_block, create_l2_transaction, execute_l2_transaction,
};
use zksync_types::{
    block::L2BlockHeader,
    commitment::L1BatchCommitmentMode,
    fee_model::{BatchFeeInput, FeeModelConfig, FeeModelConfigV2},
    l2::L2Tx,
    protocol_version::{L1VerifierConfig, ProtocolSemanticVersion},
    system_contracts::get_system_smart_contracts,
    L2BlockNumber, L2ChainId, PriorityOpId, ProtocolVersionId, H256,
};

use crate::{MempoolGuard, MempoolIO};

#[derive(Debug)]
pub struct Tester {
    base_system_contracts: BaseSystemContracts,
    current_timestamp: u64,
    _commitment_mode: L1BatchCommitmentMode,
}

impl Tester {
    pub(super) fn new(commitment_mode: L1BatchCommitmentMode) -> Self {
        let base_system_contracts = BaseSystemContracts::load_from_disk();
        Self {
            base_system_contracts,
            current_timestamp: 0,
            _commitment_mode: commitment_mode,
        }
    }

    pub(super) async fn create_batch_fee_input_provider(&self) -> ViaMainNodeFeeInputProvider {
        let inscriber = get_mock_inscriber_and_conditions(MockBitcoinOpsConfig::default());
        let config = FeeModelConfigV2 {
            minimal_l2_gas_price: 100_000_000_000,
            compute_overhead_part: 0.0,
            pubdata_overhead_part: 1.0,
            batch_overhead_l1_gas: 700_000,
            max_gas_per_batch: 500_000_000,
            max_pubdata_per_batch: 100_000,
        };
        inscriber.get_client().await;
        let client = BitcoinClient::new(
            "",
            via_btc_client::types::NodeAuth::None,
            ViaBtcClientConfig::for_tests(),
        )
        .unwrap();
        ViaMainNodeFeeInputProvider::new(
            Arc::new(
                ViaGasAdjuster::new(
                    GasAdjusterConfig {
                        internal_enforced_pubdata_price: Some(10),
                        ..Default::default()
                    },
                    Arc::new(client),
                )
                .await
                .unwrap(),
            ),
            FeeModelConfig::V2(config),
        )
        .unwrap()
    }

    // Constant value to be used both in tests and inside of the IO.
    pub(super) fn minimal_l2_gas_price(&self) -> u64 {
        100
    }

    pub(super) async fn create_test_mempool_io(
        &self,
        pool: ConnectionPool<Core>,
    ) -> (MempoolIO, MempoolGuard) {
        let batch_fee_input_provider = self.create_batch_fee_input_provider().await;

        let mempool = MempoolGuard::new(PriorityOpId(0), 100);
        let config = StateKeeperConfig {
            minimal_l2_gas_price: self.minimal_l2_gas_price(),
            validation_computational_gas_limit: BATCH_COMPUTATIONAL_GAS_LIMIT,
            ..StateKeeperConfig::for_tests()
        };
        let wallets = Wallets::for_tests();
        let io = MempoolIO::new(
            mempool.clone(),
            Arc::new(batch_fee_input_provider),
            pool,
            &config,
            wallets.state_keeper.unwrap().fee_account.address(),
            Duration::from_secs(1),
            L2ChainId::from(270),
        )
        .unwrap();

        (io, mempool)
    }

    pub(super) fn set_timestamp(&mut self, timestamp: u64) {
        self.current_timestamp = timestamp;
    }

    pub(super) async fn genesis(&self, pool: &ConnectionPool<Core>) {
        let mut storage = pool.connection_tagged("state_keeper").await.unwrap();
        if storage.blocks_dal().is_genesis_needed().await.unwrap() {
            create_genesis_l1_batch(
                &mut storage,
                L2ChainId::max(),
                ProtocolSemanticVersion {
                    minor: ProtocolVersionId::latest(),
                    patch: 0.into(),
                },
                &self.base_system_contracts,
                &get_system_smart_contracts(),
                L1VerifierConfig::default(),
            )
            .await
            .unwrap();
        }
    }

    pub(super) async fn insert_l2_block(
        &self,
        pool: &ConnectionPool<Core>,
        number: u32,
        base_fee_per_gas: u64,
        fee_input: BatchFeeInput,
    ) -> TransactionExecutionResult {
        let mut storage = pool.connection_tagged("state_keeper").await.unwrap();
        let tx = create_l2_transaction(10, 100);
        storage
            .transactions_dal()
            .insert_transaction_l2(&tx, TransactionExecutionMetrics::default())
            .await
            .unwrap();
        storage
            .blocks_dal()
            .insert_l2_block(&L2BlockHeader {
                timestamp: self.current_timestamp,
                base_fee_per_gas,
                batch_fee_input: fee_input,
                base_system_contracts_hashes: self.base_system_contracts.hashes(),
                ..create_l2_block(number)
            })
            .await
            .unwrap();
        let tx_result = execute_l2_transaction(tx.clone());
        storage
            .transactions_dal()
            .mark_txs_as_executed_in_l2_block(
                L2BlockNumber(number),
                slice::from_ref(&tx_result),
                1.into(),
                ProtocolVersionId::latest(),
                false,
            )
            .await
            .unwrap();
        tx_result
    }

    pub(super) async fn insert_sealed_batch(
        &self,
        pool: &ConnectionPool<Core>,
        number: u32,
        tx_results: &[TransactionExecutionResult],
    ) {
        let batch_header = create_l1_batch(number);
        let mut storage = pool.connection_tagged("state_keeper").await.unwrap();
        storage
            .blocks_dal()
            .insert_mock_l1_batch(&batch_header)
            .await
            .unwrap();
        storage
            .blocks_dal()
            .mark_l2_blocks_as_executed_in_l1_batch(batch_header.number)
            .await
            .unwrap();
        storage
            .transactions_dal()
            .mark_txs_as_executed_in_l1_batch(batch_header.number, tx_results)
            .await
            .unwrap();
        storage
            .blocks_dal()
            .set_l1_batch_hash(batch_header.number, H256::default())
            .await
            .unwrap();
    }

    pub(super) fn insert_tx(
        &self,
        guard: &mut MempoolGuard,
        fee_per_gas: u64,
        gas_per_pubdata: u32,
    ) -> L2Tx {
        let tx = create_l2_transaction(fee_per_gas, gas_per_pubdata.into());
        guard.insert(vec![tx.clone().into()], Default::default());
        tx
    }
}
