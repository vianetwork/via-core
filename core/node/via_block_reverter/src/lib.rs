use std::{path::Path, sync::Arc};

use anyhow::Context as _;
use tokio::{fs, sync::Semaphore};
use zksync_dal::{ConnectionPool, Core, CoreDal};
// Public re-export to simplify the API use.
use zksync_merkle_tree::domain::ZkSyncTree;
use zksync_object_store::{ObjectStore, ObjectStoreError};
use zksync_state::RocksdbStorage;
use zksync_storage::RocksDB;
use zksync_types::{
    snapshots::{
        SnapshotFactoryDependencies, SnapshotMetadata, SnapshotStorageLogsChunk,
        SnapshotStorageLogsStorageKey,
    },
    L1BatchNumber, H256,
};

#[cfg(test)]
mod tests;

/// Role of the node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NodeRole {
    Main,
    External,
}

/// This struct is used to roll back node state and revert batches committed (but generally not finalized) on L1.
///
/// Reversion is a rare event of manual intervention, when the node operator
/// decides to revert some of the not yet finalized batches for some reason
/// (e.g. inability to generate a proof).
///
/// It is also used to automatically roll back the external node state
/// after it detects a reorg on the main node. Notice the difference in terminology; from the network
/// perspective, *reversion* leaves a trace (L1 transactions etc.), while from the perspective of the node state, a *rollback*
/// leaves the node in the same state as if the rolled back batches were never sealed in the first place.
///
/// `BlockReverter` can roll back the following pieces of node state:
///
/// - State of the Postgres database
/// - State of the Merkle tree
/// - State of the RocksDB storage cache
/// - Object store for protocol snapshots
#[derive(Debug)]
pub struct ViaBlockReverter {
    /// Role affects the interactions with the consensus state.
    /// This distinction will be removed once consensus genesis is moved to the L1 state.
    node_role: NodeRole,
    allow_rolling_back_executed_batches: bool,
    connection_pool: ConnectionPool<Core>,
    should_roll_back_postgres: bool,
    storage_cache_paths: Vec<String>,
    merkle_tree_path: Option<String>,
    snapshots_object_store: Option<Arc<dyn ObjectStore>>,
}

impl ViaBlockReverter {
    pub fn new(node_role: NodeRole, connection_pool: ConnectionPool<Core>) -> Self {
        Self {
            node_role,
            allow_rolling_back_executed_batches: false,
            connection_pool,
            should_roll_back_postgres: false,
            storage_cache_paths: Vec::new(),
            merkle_tree_path: None,
            snapshots_object_store: None,
        }
    }

    /// Allows rolling back the state past the last batch finalized on L1. If this is disallowed (which is the default),
    /// block reverter will error upon such an attempt.
    ///
    /// Main use case for the setting this flag is the external node, where may obtain an
    /// incorrect state even for a block that was marked as executed. On the EN, this mode is not destructive.
    pub fn allow_rolling_back_executed_batches(&mut self) -> &mut Self {
        self.allow_rolling_back_executed_batches = true;
        self
    }

    pub fn enable_rolling_back_postgres(&mut self) -> &mut Self {
        self.should_roll_back_postgres = true;
        self
    }

    pub fn enable_rolling_back_merkle_tree(&mut self, path: String) -> &mut Self {
        self.merkle_tree_path = Some(path);
        self
    }

    pub fn add_rocksdb_storage_path_to_rollback(&mut self, path: String) -> &mut Self {
        self.storage_cache_paths.push(path);
        self
    }

    pub fn enable_rolling_back_snapshot_objects(
        &mut self,
        object_store: Arc<dyn ObjectStore>,
    ) -> &mut Self {
        self.snapshots_object_store = Some(object_store);
        self
    }

    /// Rolls back previously enabled DBs (Postgres + RocksDB) and the snapshot object store to a previous state.
    pub async fn roll_back(&self, last_l1_batch_to_keep: L1BatchNumber) -> anyhow::Result<()> {
        if !self.allow_rolling_back_executed_batches {
            let mut storage = self.connection_pool.connection().await?;
            if let Some(first_l1_batch_can_be_reverted) = storage
                .via_blocks_dal()
                .get_first_l1_batch_can_be_reverted()
                .await?
            {
                anyhow::ensure!(
                    last_l1_batch_to_keep < first_l1_batch_can_be_reverted,
                    "Attempt to roll back already CommitProofOnchain/executed L1 batches ; the last commited proof \
                    or finalized batch is: {:?}",
                    first_l1_batch_can_be_reverted -1
                );
            } else {
                tracing::warn!("There are no L1 batches available for reversion.");
                return Ok(());
            }
        }

        // Tree needs to be rolled back first to keep the state recoverable
        self.roll_back_rocksdb_instances(last_l1_batch_to_keep)
            .await?;
        let deleted_snapshots = if self.should_roll_back_postgres {
            self.roll_back_postgres(last_l1_batch_to_keep).await?
        } else {
            vec![]
        };

        if let Some(object_store) = self.snapshots_object_store.as_deref() {
            Self::delete_snapshot_files(object_store, &deleted_snapshots).await?;
        } else if !deleted_snapshots.is_empty() {
            tracing::info!(
                "Did not remove snapshot files in object store since it was not provided; \
                 metadata for deleted snapshots: {deleted_snapshots:?}"
            );
        }

        Ok(())
    }

    async fn roll_back_rocksdb_instances(
        &self,
        last_l1_batch_to_keep: L1BatchNumber,
    ) -> anyhow::Result<()> {
        if let Some(merkle_tree_path) = &self.merkle_tree_path {
            let storage_root_hash = self
                .connection_pool
                .connection()
                .await?
                .blocks_dal()
                .get_l1_batch_state_root(last_l1_batch_to_keep)
                .await?
                .context("no state root hash for target L1 batch")?;

            let merkle_tree_path = Path::new(merkle_tree_path);
            let merkle_tree_exists = fs::try_exists(merkle_tree_path).await.with_context(|| {
                format!(
                    "cannot check whether Merkle tree path `{}` exists",
                    merkle_tree_path.display()
                )
            })?;
            if merkle_tree_exists {
                tracing::info!(
                    "Rolling back Merkle tree at `{}`",
                    merkle_tree_path.display()
                );
                let merkle_tree_path = merkle_tree_path.to_path_buf();
                tokio::task::spawn_blocking(move || {
                    Self::roll_back_tree_blocking(
                        last_l1_batch_to_keep,
                        &merkle_tree_path,
                        storage_root_hash,
                    )
                })
                .await
                .context("rolling back Merkle tree panicked")??;
            } else {
                tracing::info!(
                    "Merkle tree not found at `{}`; skipping",
                    merkle_tree_path.display()
                );
            }
        }

        for storage_cache_path in &self.storage_cache_paths {
            let sk_cache_exists = fs::try_exists(storage_cache_path).await.with_context(|| {
                format!("cannot check whether storage cache path `{storage_cache_path}` exists")
            })?;
            anyhow::ensure!(
                sk_cache_exists,
                "Path with storage cache DB doesn't exist at `{storage_cache_path}`"
            );
            self.roll_back_storage_cache(last_l1_batch_to_keep, storage_cache_path)
                .await?;
        }
        Ok(())
    }

    fn roll_back_tree_blocking(
        last_l1_batch_to_keep: L1BatchNumber,
        path: &Path,
        storage_root_hash: H256,
    ) -> anyhow::Result<()> {
        let db = RocksDB::new(path).context("failed initializing RocksDB for Merkle tree")?;
        let mut tree =
            ZkSyncTree::new_lightweight(db.into()).context("failed initializing Merkle tree")?;

        if tree.next_l1_batch_number() <= last_l1_batch_to_keep {
            tracing::info!("Tree is behind the L1 batch to roll back to; skipping");
            return Ok(());
        }
        tree.roll_back_logs(last_l1_batch_to_keep)
            .context("cannot roll back Merkle tree")?;

        tracing::info!("Checking match of the tree root hash and root hash from Postgres");
        let tree_root_hash = tree.root_hash();
        anyhow::ensure!(
            tree_root_hash == storage_root_hash,
            "Mismatch between the tree root hash {tree_root_hash:?} and storage root hash {storage_root_hash:?} after rollback"
        );
        tracing::info!("Saving tree changes to disk");
        tree.save().context("failed saving tree changes")?;
        Ok(())
    }

    /// Rolls back changes in the storage cache.
    async fn roll_back_storage_cache(
        &self,
        last_l1_batch_to_keep: L1BatchNumber,
        storage_cache_path: &str,
    ) -> anyhow::Result<()> {
        tracing::info!("Opening DB with storage cache at `{storage_cache_path}`");
        let sk_cache = RocksdbStorage::builder(storage_cache_path.as_ref())
            .await
            .context("failed initializing storage cache")?;

        if sk_cache.l1_batch_number().await > Some(last_l1_batch_to_keep + 1) {
            let mut storage = self.connection_pool.connection().await?;
            tracing::info!("Rolling back storage cache");
            sk_cache
                .roll_back(&mut storage, last_l1_batch_to_keep)
                .await
                .context("failed rolling back storage cache")?;
        } else {
            tracing::info!("Nothing to roll back in storage cache");
        }
        Ok(())
    }

    /// Rolls back data in the Postgres database.
    /// If `node_role` is `Main` a consensus hard-fork is performed.
    async fn roll_back_postgres(
        &self,
        last_l1_batch_to_keep: L1BatchNumber,
    ) -> anyhow::Result<Vec<SnapshotMetadata>> {
        tracing::info!("Rolling back Postgres data");
        let mut storage = self.connection_pool.connection().await?;
        let mut transaction = storage.start_transaction().await?;
        let (_, last_l2_block_to_keep) = transaction
            .blocks_dal()
            .get_l2_block_range_of_l1_batch(last_l1_batch_to_keep)
            .await?
            .with_context(|| {
                format!("L1 batch #{last_l1_batch_to_keep} doesn't contain L2 blocks")
            })?;

        tracing::info!("Rolling back transactions state");
        transaction
            .transactions_dal()
            .reset_transactions_state(last_l2_block_to_keep)
            .await?;
        tracing::info!("Rolling back events");
        transaction
            .events_dal()
            .roll_back_events(last_l2_block_to_keep)
            .await?;
        tracing::info!("Rolling back L2-to-L1 logs");
        transaction
            .events_dal()
            .roll_back_l2_to_l1_logs(last_l2_block_to_keep)
            .await?;
        tracing::info!("Rolling back created tokens");
        transaction
            .tokens_dal()
            .roll_back_tokens(last_l2_block_to_keep)
            .await?;
        tracing::info!("Rolling back factory deps");
        transaction
            .factory_deps_dal()
            .roll_back_factory_deps(last_l2_block_to_keep)
            .await?;
        tracing::info!("Rolling back storage logs");
        transaction
            .storage_logs_dal()
            .roll_back_storage_logs(last_l2_block_to_keep)
            .await?;
        tracing::info!("Rolling back snapshots");
        let deleted_snapshots = transaction
            .snapshots_dal()
            .delete_snapshots_after(last_l1_batch_to_keep)
            .await?;

        // Remove data from main tables (L2 blocks and L1 batches).
        tracing::info!("Rolling back L1 batches");
        transaction
            .blocks_dal()
            .delete_l1_batches(last_l1_batch_to_keep)
            .await?;
        tracing::info!("Rolling back initial writes");
        transaction
            .blocks_dal()
            .delete_initial_writes(last_l1_batch_to_keep)
            .await?;
        tracing::info!("Rolling back vm_runner_protective_reads");
        transaction
            .vm_runner_dal()
            .delete_protective_reads(last_l1_batch_to_keep)
            .await?;
        tracing::info!("Rolling back vm_runner_bwip");
        transaction
            .vm_runner_dal()
            .delete_bwip_data(last_l1_batch_to_keep)
            .await?;
        tracing::info!("Rolling back L2 blocks");
        transaction
            .blocks_dal()
            .delete_l2_blocks(last_l2_block_to_keep)
            .await?;

        if self.node_role == NodeRole::Main {
            tracing::info!("Performing consensus hard fork");
            transaction.consensus_dal().fork().await?;
        }

        transaction.commit().await?;
        Ok(deleted_snapshots)
    }

    async fn delete_snapshot_files(
        object_store: &dyn ObjectStore,
        deleted_snapshots: &[SnapshotMetadata],
    ) -> anyhow::Result<()> {
        const CONCURRENT_REMOVE_REQUESTS: usize = 20;

        fn ignore_not_found_errors(err: ObjectStoreError) -> Result<(), ObjectStoreError> {
            match err {
                ObjectStoreError::KeyNotFound(err) => {
                    tracing::debug!("Ignoring 'not found' object store error: {err}");
                    Ok(())
                }
                _ => Err(err),
            }
        }

        fn combine_results(output: &mut anyhow::Result<()>, result: anyhow::Result<()>) {
            if let Err(err) = result {
                tracing::warn!("{err:?}");
                *output = Err(err);
            }
        }

        if deleted_snapshots.is_empty() {
            return Ok(());
        }

        let mut overall_result = Ok(());
        for snapshot in deleted_snapshots {
            tracing::info!(
                "Removing factory deps for snapshot for L1 batch #{}",
                snapshot.l1_batch_number
            );
            let result = object_store
                .remove::<SnapshotFactoryDependencies>(snapshot.l1_batch_number)
                .await
                .or_else(ignore_not_found_errors)
                .with_context(|| {
                    format!(
                        "failed removing factory deps for snapshot for L1 batch #{}",
                        snapshot.l1_batch_number
                    )
                });
            combine_results(&mut overall_result, result);

            let mut is_incomplete_snapshot = false;
            let chunk_ids_iter = (0_u64..)
                .zip(&snapshot.storage_logs_filepaths)
                .filter_map(|(chunk_id, path)| {
                    if path.is_none() {
                        if !is_incomplete_snapshot {
                            is_incomplete_snapshot = true;
                            tracing::warn!(
                                "Snapshot for L1 batch #{} is incomplete (misses al least storage logs chunk ID {chunk_id}). \
                                 It is probable that it's currently being created, in which case you'll need to clean up produced files \
                                 manually afterwards",
                                snapshot.l1_batch_number
                            );
                        }
                        return None;
                    }
                    Some(chunk_id)
                });

            let remove_semaphore = &Semaphore::new(CONCURRENT_REMOVE_REQUESTS);
            let remove_futures = chunk_ids_iter.map(|chunk_id| async move {
                let _permit = remove_semaphore
                    .acquire()
                    .await
                    .context("semaphore is never closed")?;

                let key = SnapshotStorageLogsStorageKey {
                    l1_batch_number: snapshot.l1_batch_number,
                    chunk_id,
                };
                tracing::info!("Removing storage logs chunk {key:?}");
                object_store
                    .remove::<SnapshotStorageLogsChunk>(key)
                    .await
                    .or_else(ignore_not_found_errors)
                    .with_context(|| format!("failed removing storage logs chunk {key:?}"))
            });
            let remove_results = futures::future::join_all(remove_futures).await;

            for result in remove_results {
                combine_results(&mut overall_result, result);
            }
        }
        overall_result
    }
}
