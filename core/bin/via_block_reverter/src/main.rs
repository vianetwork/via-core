use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use tokio::{
    fs,
    io::{self, AsyncReadExt},
};
use via_block_reverter::{NodeRole, ViaBlockReverter};
use zksync_config::configs::chain::TimestampAsserterConfig;
use zksync_config::{
    configs::{
        chain::{CircuitBreakerConfig, MempoolConfig, OperationsManagerConfig, StateKeeperConfig},
        fri_prover_group::FriProverGroupConfig,
        house_keeper::HouseKeeperConfig,
        BasicWitnessInputProducerConfig, DatabaseSecrets, ExperimentalVmConfig,
        ExternalPriceApiClientConfig, FriProofCompressorConfig, FriProverConfig,
        FriProverGatewayConfig, FriWitnessGeneratorConfig, FriWitnessVectorGeneratorConfig,
        GeneralConfig, ObservabilityConfig, PrometheusConfig, ProofDataHandlerConfig,
        ProtectiveReadsWriterConfig, ProverJobMonitorConfig,
    },
    ApiConfig, BaseTokenAdjusterConfig, DADispatcherConfig, DBConfig,
    ExternalProofIntegrationApiConfig, ObjectStoreConfig, PostgresConfig, SnapshotsCreatorConfig,
};
use zksync_core_leftovers::temp_config_store::read_yaml_repr;
use zksync_dal::{ConnectionPool, Core};
use zksync_env_config::{object_store::SnapshotsObjectStoreConfig, FromEnv};
use zksync_object_store::ObjectStoreFactory;
use zksync_types::L1BatchNumber;

#[derive(Debug, Parser)]
#[command(author = "Matter Labs", version, about = "Block revert utility", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Path to yaml config. If set, it will be used instead of env vars
    #[arg(long, global = true)]
    config_path: Option<PathBuf>,
    /// Path to yaml secrets config. If set, it will be used instead of env vars
    #[arg(long, global = true)]
    secrets_path: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Rolls back internal database state to a previous L1 batch.
    #[command(name = "rollback-db")]
    RollbackDB {
        /// L1 batch number used to roll back to.
        #[arg(long)]
        l1_batch_number: u32,
        /// Flag that specifies if Postgres DB should be rolled back.
        #[arg(long)]
        rollback_postgres: bool,
        /// Flag that specifies if RocksDB with tree should be rolled back.
        #[arg(long)]
        rollback_tree: bool,
        /// Flag that specifies if RocksDB with state keeper cache should be rolled back.
        #[arg(long)]
        rollback_sk_cache: bool,
        /// Flag that specifies if RocksDBs with vm runners' caches should be rolled back.
        #[arg(long)]
        rollback_vm_runners_cache: bool,
        /// Flag that specifies if snapshot files in GCS should be rolled back.
        #[arg(long, requires = "rollback_postgres")]
        rollback_snapshots: bool,
        /// Flag that allows to roll back already executed blocks. It's ultra dangerous and required only for fixing external nodes.
        #[arg(long)]
        allow_executed_block_reversion: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = Cli::parse();

    let observability_config =
        ObservabilityConfig::from_env().context("ObservabilityConfig::from_env()")?;

    let logs = zksync_vlog::Logs::try_from(observability_config.clone())
        .context("logs")?
        .disable_default_logs(); // It's a CLI application, so we only need to show logs that were actually requested.;
    let sentry: Option<zksync_vlog::Sentry> =
        TryFrom::try_from(observability_config.clone()).context("sentry")?;
    let opentelemetry: Option<zksync_vlog::OpenTelemetry> =
        TryFrom::try_from(observability_config.clone()).context("opentelemetry")?;
    let _guard = zksync_vlog::ObservabilityBuilder::new()
        .with_logs(Some(logs))
        .with_sentry(sentry)
        .with_opentelemetry(opentelemetry)
        .build();

    let general_config: Option<GeneralConfig> = if let Some(path) = opts.config_path {
        let config = read_yaml_repr::<zksync_protobuf_config::proto::general::GeneralConfig>(&path)
            .context("failed decoding general YAML config")?;
        Some(config)
    } else {
        Some(load_env_config()?)
    };

    let db_config = match &general_config {
        Some(general_config) => general_config
            .db_config
            .clone()
            .context("Failed to find eth config")?,
        None => DBConfig::from_env().context("DBConfig::from_env()")?,
    };

    let protective_reads_writer_config = match &general_config {
        Some(general_config) => general_config
            .protective_reads_writer_config
            .clone()
            .context("Failed to find eth config")?,
        None => ProtectiveReadsWriterConfig::from_env()
            .context("ProtectiveReadsWriterConfig::from_env()")?,
    };

    let basic_witness_input_producer_config = match &general_config {
        Some(general_config) => general_config
            .basic_witness_input_producer_config
            .clone()
            .context("Failed to find eth config")?,
        None => BasicWitnessInputProducerConfig::from_env()
            .context("BasicWitnessInputProducerConfig::from_env()")?,
    };

    let secrets_config = if let Some(path) = opts.secrets_path {
        let config = read_yaml_repr::<zksync_protobuf_config::proto::secrets::Secrets>(&path)
            .context("failed decoding secrets YAML config")?;
        Some(config)
    } else {
        None
    };

    let database_secrets = match &secrets_config {
        Some(secrets_config) => secrets_config
            .database
            .clone()
            .context("Failed to find database config")?,
        None => DatabaseSecrets::from_env().context("DatabaseSecrets::from_env()")?,
    };

    let postgres_config = match &general_config {
        Some(general_config) => general_config
            .postgres_config
            .clone()
            .context("Failed to find postgres config")?,
        None => PostgresConfig::from_env().context("PostgresConfig::from_env()")?,
    };

    let connection_pool = ConnectionPool::<Core>::builder(
        database_secrets.master_url()?,
        postgres_config.max_connections()?,
    )
    .build()
    .await
    .context("failed to build a connection pool")?;
    let mut block_reverter = ViaBlockReverter::new(NodeRole::Main, connection_pool);

    match opts.command {
        Command::RollbackDB {
            l1_batch_number,
            rollback_postgres,
            rollback_tree,
            rollback_sk_cache,
            rollback_vm_runners_cache,
            rollback_snapshots,
            allow_executed_block_reversion,
        } => {
            if !rollback_tree && rollback_postgres {
                println!("You want to roll back Postgres DB without rolling back tree.");
                println!(
                    "If the tree is not yet rolled back to this L1 batch, then the only way \
                     to make it synced with Postgres will be to completely rebuild it."
                );
                println!("Are you sure? Print y/n");

                let mut input = [0u8];
                io::stdin().read_exact(&mut input).await.unwrap();
                if input[0] != b'y' && input[0] != b'Y' {
                    std::process::exit(0);
                }
            }

            if allow_executed_block_reversion {
                println!("You want to roll back already executed blocks. It's impossible to restore them for the main node");
                println!("Make sure you are doing it ONLY for external node");
                println!("Are you sure? Print y/n");

                let mut input = [0u8];
                io::stdin().read_exact(&mut input).await.unwrap();
                if input[0] != b'y' && input[0] != b'Y' {
                    std::process::exit(0);
                }
                block_reverter.allow_rolling_back_executed_batches();
            }

            if rollback_postgres {
                block_reverter.enable_rolling_back_postgres();
                if rollback_snapshots {
                    let object_store_config = SnapshotsObjectStoreConfig::from_env()
                        .context("SnapshotsObjectStoreConfig::from_env()")?;
                    block_reverter.enable_rolling_back_snapshot_objects(
                        ObjectStoreFactory::new(object_store_config.0)
                            .create_store()
                            .await?,
                    );
                }
            }
            if rollback_tree {
                block_reverter.enable_rolling_back_merkle_tree(db_config.merkle_tree.path);
            }
            if rollback_sk_cache {
                block_reverter.add_rocksdb_storage_path_to_rollback(db_config.state_keeper_db_path);
            }

            if rollback_vm_runners_cache {
                let cache_exists = fs::try_exists(&protective_reads_writer_config.db_path)
                    .await
                    .with_context(|| {
                        format!(
                            "cannot check whether storage cache path `{}` exists",
                            protective_reads_writer_config.db_path
                        )
                    })?;
                if cache_exists {
                    block_reverter.add_rocksdb_storage_path_to_rollback(
                        protective_reads_writer_config.db_path,
                    );
                }

                let cache_exists = fs::try_exists(&basic_witness_input_producer_config.db_path)
                    .await
                    .with_context(|| {
                        format!(
                            "cannot check whether storage cache path `{}` exists",
                            basic_witness_input_producer_config.db_path
                        )
                    })?;
                if cache_exists {
                    block_reverter.add_rocksdb_storage_path_to_rollback(
                        basic_witness_input_producer_config.db_path,
                    );
                }
            }

            block_reverter
                .roll_back(L1BatchNumber(l1_batch_number))
                .await?;
        }
    }
    Ok(())
}

pub(crate) fn load_env_config() -> anyhow::Result<GeneralConfig> {
    Ok(GeneralConfig {
        postgres_config: PostgresConfig::from_env().ok(),
        api_config: ApiConfig::from_env().ok(),
        contract_verifier: None,
        circuit_breaker_config: CircuitBreakerConfig::from_env().ok(),
        mempool_config: MempoolConfig::from_env().ok(),
        operations_manager_config: OperationsManagerConfig::from_env().ok(),
        state_keeper_config: StateKeeperConfig::from_env().ok(),
        house_keeper_config: HouseKeeperConfig::from_env().ok(),
        proof_compressor_config: FriProofCompressorConfig::from_env().ok(),
        prover_config: FriProverConfig::from_env().ok(),
        prover_gateway: FriProverGatewayConfig::from_env().ok(),
        witness_vector_generator: FriWitnessVectorGeneratorConfig::from_env().ok(),
        prover_group_config: FriProverGroupConfig::from_env().ok(),
        witness_generator_config: FriWitnessGeneratorConfig::from_env().ok(),
        prometheus_config: PrometheusConfig::from_env().ok(),
        proof_data_handler_config: ProofDataHandlerConfig::from_env().ok(),
        db_config: DBConfig::from_env().ok(),
        eth: None,
        snapshot_creator: SnapshotsCreatorConfig::from_env().ok(),
        observability: ObservabilityConfig::from_env().ok(),
        da_dispatcher_config: DADispatcherConfig::from_env().ok(),
        protective_reads_writer_config: ProtectiveReadsWriterConfig::from_env().ok(),
        basic_witness_input_producer_config: BasicWitnessInputProducerConfig::from_env().ok(),
        commitment_generator: None,
        snapshot_recovery: None,
        pruning: None,
        core_object_store: ObjectStoreConfig::from_env().ok(),
        base_token_adjuster: BaseTokenAdjusterConfig::from_env().ok(),
        external_price_api_client_config: ExternalPriceApiClientConfig::from_env().ok(),
        consensus_config: None,
        external_proof_integration_api_config: ExternalProofIntegrationApiConfig::from_env().ok(),
        experimental_vm_config: ExperimentalVmConfig::from_env().ok(),
        prover_job_monitor_config: ProverJobMonitorConfig::from_env().ok(),
        da_client_config: None,
        timestamp_asserter_config: TimestampAsserterConfig::from_env().ok(),
    })
}
