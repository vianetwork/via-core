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
        types::L1ToL2MessageInput,
    };
    use via_btc_storage::{ReaderDal, PROTOCOL_CONSUMER};
    use via_reorg::chain::{BlockRef, Header};
    use via_test_utils::bitcoin_core::BitcoinCore;
    use zksync_multivm::interface::storage::InMemoryStorage;
    use zksync_node_genesis::{insert_genesis_batch, GenesisParams};
    use zksync_types::{l1::via_l1::ViaL1Deposit, Address, H256};

    use super::*;

    #[tokio::test]
    async fn cached_priority_execution_requires_current_source_and_observes_stop(
    ) -> anyhow::Result<()> {
        let core = BitcoinCore::new()?;
        core.mine(1)?;
        let client = core.client()?;
        let pool = ConnectionPool::<Core>::test_pool().await;
        let mut db = pool.connection().await?;
        insert_genesis_batch(&mut db, &GenesisParams::mock()).await?;
        let genesis = client.canonical_header_at(0).await?;
        let source = client.canonical_header_at(1).await?;
        let mut tx = db.start_transaction().await?;
        let mut reader = ReaderDal::new(&mut tx);
        reader
            .initialize(
                "regtest",
                genesis.hash,
                Header { height: 0, block_hash: genesis.hash, parent_hash: genesis.parent_hash },
                BlockRef { height: 1, block_hash: source.hash },
            )
            .await?;
        let root = reader.header_at(0).await?.unwrap();
        reader
            .save_block(
                Header { height: 1, block_hash: source.hash, parent_hash: source.parent_hash },
                &root,
            )
            .await?;
        let head = reader.header_at(1).await?.unwrap();
        reader.complete_consumer(PROTOCOL_CONSUMER, &root, &root).await?;
        reader.complete_consumer(PROTOCOL_CONSUMER, &head, &root).await?;
        reader.advance_frontier(&head, &head).await?;
        sqlx::query("UPDATE protocol_patches SET bitcoin_height=0,bitcoin_hash=$1")
            .bind(genesis.hash.to_string())
            .execute(tx.conn())
            .await?;
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
        let mut factory = BitcoinExecutionFactory::new(Box::new(scripted), pool, client, receiver);
        let env = crate::tests::pending_batch_data(vec![]);
        let mut executor = factory.init_batch(
            OwnedStorage::boxed(InMemoryStorage::default()),
            env.l1_batch_env,
            env.system_env,
            env.pubdata_params,
        );
        executor.execute_tx(transaction.clone()).await?;
        sqlx::query("DELETE FROM transactions WHERE hash=$1")
            .bind(hash.as_bytes())
            .execute(db.conn())
            .await?;
        // Scripted executor has no second result: reaching it again panics, including if admission moves after execution.
        let pending = executor.execute_tx(transaction);
        tokio::pin!(pending);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(250), &mut pending)
            .await
            .is_err());
        stop.send(true)?;
        assert!(pending.await.is_err());
        Ok(())
    }
}
