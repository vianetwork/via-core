use std::sync::Arc;

use anyhow::Context;
use zksync_health_check::ReactiveHealthCheck;
use zksync_state::AsyncCatchupTask;
pub use zksync_state::RocksdbStorageOptions;
use zksync_state_keeper::{AsyncRocksdbCache, ZkSyncStateKeeper};
use zksync_storage::RocksDB;

use crate::{
    implementations::resources::{
        healthcheck::AppHealthCheckResource,
        pools::{MasterPool, PoolResource},
        state_keeper::{
            BatchExecutorResource, ConditionalSealerResource, OutputHandlerResource,
            StateKeeperIOResource,
        },
    },
    service::{ShutdownHook, StopReceiver},
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

pub mod external_io;
pub mod main_batch_executor;
pub mod mempool_io;
pub mod output_handler;

/// Wiring layer for the state keeper.
#[derive(Debug)]
pub struct StateKeeperLayer {
    state_keeper_db_path: String,
    rocksdb_options: RocksdbStorageOptions,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub state_keeper_io: StateKeeperIOResource,
    pub batch_executor: BatchExecutorResource,
    pub output_handler: OutputHandlerResource,
    pub conditional_sealer: ConditionalSealerResource,
    pub master_pool: PoolResource<MasterPool>,
    pub btc_client: Option<crate::implementations::resources::via_btc_client::BtcClientResource>,
    #[context(default)]
    pub app_health: AppHealthCheckResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub state_keeper: StateKeeperTask,
    #[context(task)]
    pub rocksdb_catchup: AsyncCatchupTask,
    pub rocksdb_termination_hook: ShutdownHook,
}

impl StateKeeperLayer {
    pub fn new(state_keeper_db_path: String, rocksdb_options: RocksdbStorageOptions) -> Self {
        Self {
            state_keeper_db_path,
            rocksdb_options,
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for StateKeeperLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "state_keeper_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let io = input
            .state_keeper_io
            .0
            .take()
            .context("StateKeeperIO was provided but taken by some other task")?;
        let batch_executor_base = input
            .batch_executor
            .0
            .take()
            .context("L1BatchExecutorBuilder was provided but taken by some other task")?;
        let output_handler = input
            .output_handler
            .0
            .take()
            .context("HandleStateKeeperOutput was provided but taken by another task")?;
        let sealer = input.conditional_sealer.0;
        let master_pool = input.master_pool;

        let (storage_factory, rocksdb_catchup) = AsyncRocksdbCache::new(
            master_pool.get_custom(2).await?,
            self.state_keeper_db_path,
            self.rocksdb_options,
        );

        let (execution_stop, execution_receiver) = tokio::sync::watch::channel(false);
        let batch_executor_base: Box<
            dyn zksync_vm_executor::interface::BatchExecutorFactory<zksync_state::OwnedStorage>,
        > = if let Some(client) = input.btc_client {
            Box::new(
                via_state_keeper::bitcoin_execution::BitcoinExecutionFactory::new(
                    batch_executor_base,
                    master_pool.get_custom(2).await?,
                    client.default,
                    execution_receiver,
                ),
            )
        } else {
            batch_executor_base
        };
        let state_keeper = ZkSyncStateKeeper::new(
            io,
            batch_executor_base,
            output_handler,
            sealer,
            Arc::new(storage_factory),
        );

        let state_keeper = StateKeeperTask {
            state_keeper,
            execution_stop,
        };

        input
            .app_health
            .0
            .insert_component(state_keeper.health_check())
            .map_err(WiringError::internal)?;

        let rocksdb_termination_hook = ShutdownHook::new("rocksdb_terminaton", async {
            // Wait for all the instances of RocksDB to be destroyed.
            tokio::task::spawn_blocking(RocksDB::await_rocksdb_termination)
                .await
                .context("failed terminating RocksDB instances")
        });
        Ok(Output {
            state_keeper,
            rocksdb_catchup,
            rocksdb_termination_hook,
        })
    }
}

#[derive(Debug)]
pub struct StateKeeperTask {
    state_keeper: ZkSyncStateKeeper,
    execution_stop: tokio::sync::watch::Sender<bool>,
}

impl StateKeeperTask {
    /// Returns the health check for state keeper.
    pub fn health_check(&self) -> ReactiveHealthCheck {
        self.state_keeper.health_check()
    }
}

#[async_trait::async_trait]
impl Task for StateKeeperTask {
    fn id(&self) -> TaskId {
        "state_keeper".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        let mut stop = stop_receiver.0.clone();
        tokio::select! {
            result = self.state_keeper.run(stop_receiver.0) => result,
            _ = stop.changed() => { let _ = self.execution_stop.send(true); Ok(()) }
        }
    }
}

// #[async_trait::async_trait]
// impl Task for AsyncCatchupTask {
//     fn kind(&self) -> TaskKind {
//         TaskKind::OneshotTask
//     }

//     fn id(&self) -> TaskId {
//         "state_keeper/rocksdb_catchup_task".into()
//     }

//     async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
//         (*self).run(stop_receiver.0).await
//     }
// }
