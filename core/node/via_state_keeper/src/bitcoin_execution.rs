use std::sync::Arc;

use anyhow::Context;
use tokio::sync::watch;
use via_btc_client::client::BitcoinClient;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_multivm::interface::{
    executor::{BatchExecutor, BatchExecutorFactory},
    storage::StorageView,
    BatchTransactionExecutionResult, FinishedL1Batch, L1BatchEnv, L2BlockEnv, SystemEnv,
};
use zksync_state::OwnedStorage;
use zksync_types::{commitment::PubdataParams, ProtocolVersionId, Transaction};

/// BitcoinExecutionFactory gates the VM used for both new transactions and cached batch replay.
/// Its transaction retains reader/source exclusion through each VM action. Source failure waits cancellably.
/// Idea only from etcd's IsOwner guard, which holds checked ownership around the protected action: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/client/v3/concurrency/mutex.go
#[derive(Debug)]
pub struct BitcoinExecutionFactory {
    inner: Box<dyn BatchExecutorFactory<OwnedStorage>>,
    gate: ExecutionGate,
}
#[derive(Debug, Clone)]
struct ExecutionGate {
    pool: ConnectionPool<Core>,
    client: Arc<BitcoinClient>,
    stop: watch::Receiver<bool>,
}
impl BitcoinExecutionFactory {
    pub fn new(
        inner: Box<dyn BatchExecutorFactory<OwnedStorage>>,
        pool: ConnectionPool<Core>,
        client: Arc<BitcoinClient>,
        stop: watch::Receiver<bool>,
    ) -> Self {
        Self { inner, gate: ExecutionGate { pool, client, stop } }
    }
}
impl BatchExecutorFactory<OwnedStorage> for BitcoinExecutionFactory {
    fn init_batch(
        &mut self,
        storage: OwnedStorage,
        l1_batch_params: L1BatchEnv,
        system_env: SystemEnv,
        pubdata_params: PubdataParams,
    ) -> Box<dyn BatchExecutor<OwnedStorage>> {
        let version = system_env.version;
        Box::new(BitcoinExecution {
            version,
            gate: self.gate.clone(),
            inner: self.inner.init_batch(storage, l1_batch_params, system_env, pubdata_params),
        })
    }
}
impl ExecutionGate {
    async fn acquire(
        &self,
        version: ProtocolVersionId,
        transaction: Option<&Transaction>,
    ) -> anyhow::Result<Connection<'static, Core>> {
        let mut stop = self.stop.clone();
        loop {
            anyhow::ensure!(!*stop.borrow(), "state keeper stopped");
            let attempt = async {
                let mut guard = via_btc_storage::transition::begin_authority(
                    &self.pool,
                    self.client.as_ref(),
                    &[],
                )
                .await?;
                anyhow::ensure!(
                    guard.via_transactions_dal().protocol_is_mature(version).await?,
                    via_btc_storage::StoreError::AuthorityHeld
                );
                if let Some(transaction) = transaction {
                    anyhow::ensure!(
                        guard.via_transactions_dal().transaction_is_mature(transaction).await?,
                        via_btc_storage::StoreError::AuthorityHeld
                    );
                }
                Ok::<_, anyhow::Error>(guard)
            };
            match tokio::select! { result = attempt => result, _ = stop.changed() => anyhow::bail!("state keeper stopped") }
            {
                Ok(guard) => return Ok(guard),
                Err(error) => {
                    if !via_btc_storage::transition::is_held(&error) {
                        return Err(error);
                    }
                    tracing::debug!(%error, "Bitcoin execution authority held");
                }
            }
            tokio::select! { _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}, _ = stop.changed() => {} }
        }
    }
}
#[derive(Debug)]
struct BitcoinExecution {
    inner: Box<dyn BatchExecutor<OwnedStorage>>,
    gate: ExecutionGate,
    version: ProtocolVersionId,
}
#[async_trait::async_trait]
impl BatchExecutor<OwnedStorage> for BitcoinExecution {
    async fn execute_tx(
        &mut self,
        tx: Transaction,
    ) -> anyhow::Result<BatchTransactionExecutionResult> {
        let guard = self
            .gate
            .acquire(self.version, Some(&tx))
            .await
            .context("admitting Bitcoin-dependent VM execution")?;
        let result = self.inner.execute_tx(tx).await;
        drop(guard);
        result
    }
    async fn rollback_last_tx(&mut self) -> anyhow::Result<()> {
        self.inner.rollback_last_tx().await
    }
    async fn start_next_l2_block(&mut self, env: L2BlockEnv) -> anyhow::Result<()> {
        let guard = self.gate.acquire(self.version, None).await?;
        let result = self.inner.start_next_l2_block(env).await;
        drop(guard);
        result
    }
    async fn finish_batch(
        self: Box<Self>,
    ) -> anyhow::Result<(FinishedL1Batch, StorageView<OwnedStorage>)> {
        let guard = self.gate.acquire(self.version, None).await?;
        let result = self.inner.finish_batch().await;
        drop(guard);
        result
    }
}

#[cfg(test)]
mod tests {
    use via_btc_client::{
        indexer::replay::{DepositOrigin, Payload},
        traits::BitcoinOps,
        types::L1ToL2MessageInput,
    };
    use via_btc_storage::ReaderDal;
    use via_test_utils::bitcoin_core::BitcoinCore;
    use zksync_multivm::interface::storage::InMemoryStorage;
    use zksync_node_genesis::{insert_genesis_batch, GenesisParams};
    use zksync_types::{l1::via_l1::ViaL1Deposit, Address, H256};

    use super::*;

    async fn execution_fixture() -> anyhow::Result<(
        BitcoinCore,
        ConnectionPool<Core>,
        Connection<'static, Core>,
        watch::Sender<bool>,
        Box<dyn BatchExecutor<OwnedStorage>>,
        Transaction,
    )> {
        let core = BitcoinCore::new()?;
        core.mine(1)?;
        let client = core.client()?;
        let pool = ConnectionPool::<Core>::test_pool().await;
        let mut db = pool.connection().await?;
        insert_genesis_batch(&mut db, &GenesisParams::mock()).await?;
        let genesis = client.canonical_header_at(0).await?;
        let source = client.canonical_header_at(1).await?;
        let mut tx = db.start_transaction().await?;
        sqlx::query("UPDATE protocol_patches SET bitcoin_height=0,bitcoin_hash=$1")
            .bind(genesis.hash.to_string())
            .execute(tx.conn())
            .await?;
        let mut chain = via_test_utils::chain::Chain::default();
        for height in 0..=1 {
            chain.blocks.insert(height, client.fetch_block(u128::from(height)).await?);
        }
        assert!(via_test_utils::reader_at(&mut tx, &chain, 1, 1, bitcoin::Network::Regtest).await?);
        let deposit = ViaL1Deposit {
            l2_receiver_address: Address::repeat_byte(7),
            amount: 50_000,
            calldata: vec![],
            l1_block_number: 1,
            tx_index: 0,
            output_vout: 0,
        };
        let operation = deposit.l1_tx().unwrap();
        tx.via_transactions_dal()
            .insert_transaction_l1(
                &operation,
                operation.eth_block(),
                H256::repeat_byte(9),
                &DepositOrigin { height: 1, hash: source.hash, vout: 0 },
                &Payload {
                    input: (
                        L1ToL2MessageInput {
                            receiver_l2_address: deposit.l2_receiver_address,
                            l2_contract_address: Address::zero(),
                            call_data: vec![],
                        },
                        deposit.amount,
                    ),
                    public_key: vec![],
                    signature: vec![],
                    sender_script: None,
                },
            )
            .await?;
        assert!(ReaderDal::new(&mut tx).complete_replay().await?);
        tx.commit().await?;
        let transaction: Transaction = operation.into();
        let hash = transaction.hash();
        let mut scripted =
            crate::testonly::test_batch_executor::TestBatchExecutorBuilder::default();
        scripted.push_successful_transactions(&[hash]);
        let (stop, receiver) = watch::channel(false);
        let mut factory =
            BitcoinExecutionFactory::new(Box::new(scripted), pool.clone(), client, receiver);
        let env = crate::tests::pending_batch_data(vec![]);
        let executor = factory.init_batch(
            OwnedStorage::boxed(InMemoryStorage::default()),
            env.l1_batch_env,
            env.system_env,
            env.pubdata_params,
        );
        Ok((core, pool, db, stop, executor, transaction))
    }

    async fn held<T>(
        future: impl std::future::Future<Output = anyhow::Result<T>>,
        stop: watch::Sender<bool>,
    ) {
        tokio::pin!(future);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(250), &mut future)
            .await
            .is_err());
        stop.send(true).unwrap();
        assert!(future.await.is_err());
    }

    #[tokio::test]
    async fn cached_priority_execution_requires_current_source_and_observes_stop(
    ) -> anyhow::Result<()> {
        let (_core, _pool, mut db, stop, mut executor, transaction) = execution_fixture().await?;
        let hash = transaction.hash();
        executor.execute_tx(transaction.clone()).await?;
        sqlx::query("DELETE FROM transactions WHERE hash=$1")
            .bind(hash.as_bytes())
            .execute(db.conn())
            .await?;
        held(executor.execute_tx(transaction), stop).await;
        Ok(())
    }
    #[tokio::test]
    async fn cached_priority_execution_rechecks_actual_core_membership() -> anyhow::Result<()> {
        let (core, _pool, mut db, stop, mut executor, transaction) = execution_fixture().await?;
        executor.execute_tx(transaction.clone()).await?;
        let hash: String = sqlx::query_scalar("SELECT hash FROM via_btc_headers WHERE number=1")
            .fetch_one(db.conn())
            .await?;
        core.rpc::<serde_json::Value>("invalidateblock", &[serde_json::json!(hash)])?;
        held(executor.execute_tx(transaction), stop).await;
        Ok(())
    }
    #[tokio::test]
    async fn cached_upgrade_waits_when_its_protocol_patch_loses_authority() -> anyhow::Result<()> {
        let (_core, _pool, mut db, stop, mut executor, _) = execution_fixture().await?;
        let upgrade: Transaction =
            crate::testonly::test_batch_executor::random_upgrade_tx(1).into();
        sqlx::query("UPDATE protocol_versions SET upgrade_tx_hash=$1")
            .bind(upgrade.hash().as_bytes())
            .execute(db.conn())
            .await?;
        sqlx::query("UPDATE protocol_patches SET bitcoin_hash='orphan'").execute(db.conn()).await?;
        held(executor.execute_tx(upgrade), stop).await;
        Ok(())
    }
    #[tokio::test]
    async fn block_continuation_waits_for_source_authority_and_observes_stop() -> anyhow::Result<()>
    {
        let (_core, _pool, mut db, stop, mut executor, _) = execution_fixture().await?;
        let env = crate::tests::pending_batch_data(vec![]).l1_batch_env.first_l2_block;
        executor.start_next_l2_block(env).await?;
        sqlx::query("UPDATE via_btc_reader SET replay_ready=FALSE").execute(db.conn()).await?;
        held(executor.start_next_l2_block(env), stop).await;
        Ok(())
    }
    #[tokio::test]
    async fn batch_finishing_waits_for_source_authority_and_observes_stop() -> anyhow::Result<()> {
        let (_core, _pool, mut db, stop, executor, _) = execution_fixture().await?;
        sqlx::query("UPDATE via_btc_reader SET replay_ready=FALSE").execute(db.conn()).await?;
        held(executor.finish_batch(), stop).await;
        let (_core, _pool, _db, _stop, executor, _) = execution_fixture().await?;
        executor.finish_batch().await?;
        Ok(())
    }
}
