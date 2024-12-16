use anyhow::Context;
use via_da_clients::celestia::wiring_layer::ViaCelestiaClientWiringLayer;
use zksync_config::{
    configs::{via_btc_sender::ProofSendingMode, wallets::Wallets, Secrets},
    ContractsConfig, GenesisConfig, ViaGeneralConfig,
};
use zksync_metadata_calculator::MetadataCalculatorConfig;
use zksync_node_api_server::{
    tx_sender::{ApiContracts, TxSenderConfig},
    web3::{state::InternalApiConfig, Namespace},
};
use zksync_node_framework::{
    implementations::layers::{
        circuit_breaker_checker::CircuitBreakerCheckerLayer,
        commitment_generator::CommitmentGeneratorLayer,
        healtcheck_server::HealthCheckLayer,
        logs_bloom_backfill::LogsBloomBackfillLayer,
        metadata_calculator::MetadataCalculatorLayer,
        node_storage_init::{
            main_node_strategy::MainNodeInitStrategyLayer, NodeStorageInitializerLayer,
        },
        object_store::ObjectStoreLayer,
        pools_layer::PoolsLayerBuilder,
        postgres_metrics::PostgresMetricsLayer,
        prometheus_exporter::PrometheusExporterLayer,
        query_eth_client::QueryEthClientLayer,
        sigint::SigintHandlerLayer,
        via_btc_sender::{
            aggregator::ViaBtcInscriptionAggregatorLayer, manager::ViaInscriptionManagerLayer,
        },
        via_btc_watch::BtcWatchLayer,
        via_da_dispatcher::DataAvailabilityDispatcherLayer,
        via_l1_gas::ViaL1GasLayer,
        via_state_keeper::{
            main_batch_executor::MainBatchExecutorLayer, mempool_io::MempoolIOLayer,
            output_handler::OutputHandlerLayer, RocksdbStorageOptions, StateKeeperLayer,
        },
        vm_runner::{
            bwip::BasicWitnessInputProducerLayer, protective_reads::ProtectiveReadsWriterLayer,
        },
        web3_api::{
            caches::MempoolCacheLayer,
            server::{Web3ServerLayer, Web3ServerOptionalConfig},
            tree_api_client::TreeApiClientLayer,
            tx_sender::{PostgresStorageCachesConfig, TxSenderLayer},
            tx_sink::MasterPoolSinkLayer,
        },
    },
    service::{ZkStackService, ZkStackServiceBuilder},
};
use zksync_types::settlement::SettlementMode;
use zksync_vlog::prometheus::PrometheusExporterConfig;

/// Macro that looks into a path to fetch an optional config,
/// and clones it into a variable.
macro_rules! try_load_config {
    ($path:expr) => {
        $path.as_ref().context(stringify!($path))?.clone()
    };
}

// TODO: list of upcoming layers
// - prometheus_exporter
//

pub struct ViaNodeBuilder {
    node: ZkStackServiceBuilder,
    configs: ViaGeneralConfig,
    wallets: Wallets,
    genesis_config: GenesisConfig,
    contracts_config: ContractsConfig,
    secrets: Secrets,
}

impl ViaNodeBuilder {
    pub fn new(
        via_general_config: ViaGeneralConfig,
        wallets: Wallets,
        secrets: Secrets,
        genesis_config: GenesisConfig,
        contracts_config: ContractsConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            node: ZkStackServiceBuilder::new().context("Cannot create ZkStackServiceBuilder")?,
            configs: via_general_config,
            wallets,
            genesis_config,
            contracts_config,
            secrets,
        })
    }

    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.node.runtime_handle()
    }

    fn add_sigint_handler_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(SigintHandlerLayer);
        Ok(self)
    }

    fn add_pools_layer(mut self) -> anyhow::Result<Self> {
        let config = try_load_config!(self.configs.postgres_config);
        let secrets = try_load_config!(self.secrets.database);
        let pools_layer = PoolsLayerBuilder::empty(config, secrets)
            .with_master(true)
            .with_replica(true)
            .with_prover(true) // Used by house keeper.
            .build();
        self.node.add_layer(pools_layer);
        Ok(self)
    }

    fn add_postgres_metrics_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(PostgresMetricsLayer);
        Ok(self)
    }

    fn add_object_store_layer(mut self) -> anyhow::Result<Self> {
        let object_store_config = try_load_config!(self.configs.core_object_store);
        self.node
            .add_layer(ObjectStoreLayer::new(object_store_config));
        Ok(self)
    }

    fn add_healthcheck_layer(mut self) -> anyhow::Result<Self> {
        let healthcheck_config = try_load_config!(self.configs.api_config).healthcheck;
        self.node.add_layer(HealthCheckLayer(healthcheck_config));
        Ok(self)
    }

    fn add_prometheus_exporter_layer(mut self) -> anyhow::Result<Self> {
        let prom_config = try_load_config!(self.configs.prometheus_config);
        let prom_config = PrometheusExporterConfig::pull(prom_config.listener_port);
        self.node.add_layer(PrometheusExporterLayer(prom_config));
        Ok(self)
    }

    fn add_circuit_breaker_checker_layer(mut self) -> anyhow::Result<Self> {
        let circuit_breaker_config = try_load_config!(self.configs.circuit_breaker_config);
        self.node
            .add_layer(CircuitBreakerCheckerLayer(circuit_breaker_config));
        Ok(self)
    }

    // QueryEthClientLayer is mock, it's not used in the current implementation
    fn add_query_eth_client_layer(mut self) -> anyhow::Result<Self> {
        let genesis = self.genesis_config.clone();
        let eth_config = try_load_config!(self.secrets.l1);
        let query_eth_client_layer = QueryEthClientLayer::new(
            genesis.settlement_layer_id(),
            eth_config.l1_rpc_url,
            self.configs
                .eth
                .as_ref()
                .and_then(|x| Some(x.gas_adjuster?.settlement_mode))
                .unwrap_or(SettlementMode::SettlesToL1),
        );
        self.node.add_layer(query_eth_client_layer);
        Ok(self)
    }

    fn add_storage_initialization_layer(mut self, kind: LayerKind) -> anyhow::Result<Self> {
        self.node.add_layer(MainNodeInitStrategyLayer {
            genesis: self.genesis_config.clone(),
            contracts: self.contracts_config.clone(),
        });
        let mut layer = NodeStorageInitializerLayer::new();
        if matches!(kind, LayerKind::Precondition) {
            layer = layer.as_precondition();
        }
        self.node.add_layer(layer);
        Ok(self)
    }

    // VIA related layers
    fn add_btc_watcher_layer(mut self) -> anyhow::Result<Self> {
        let btc_watch_config = try_load_config!(self.configs.via_btc_watch_config);
        self.node.add_layer(BtcWatchLayer::new(btc_watch_config));
        Ok(self)
    }

    fn add_btc_sender_layer(mut self) -> anyhow::Result<Self> {
        let btc_sender_config = try_load_config!(self.configs.via_btc_sender_config);
        self.node.add_layer(ViaBtcInscriptionAggregatorLayer::new(
            btc_sender_config.clone(),
        ));
        self.node
            .add_layer(ViaInscriptionManagerLayer::new(btc_sender_config));
        Ok(self)
    }

    fn add_l1_gas_layer(mut self) -> anyhow::Result<Self> {
        let state_keeper_config = try_load_config!(self.configs.state_keeper_config);
        let l1_gas_layer = ViaL1GasLayer::new(state_keeper_config);
        self.node.add_layer(l1_gas_layer);
        Ok(self)
    }

    fn add_tx_sender_layer(mut self) -> anyhow::Result<Self> {
        let sk_config = try_load_config!(self.configs.state_keeper_config);
        let rpc_config = try_load_config!(self.configs.api_config).web3_json_rpc;
        let postgres_storage_caches_config = PostgresStorageCachesConfig {
            factory_deps_cache_size: rpc_config.factory_deps_cache_size() as u64,
            initial_writes_cache_size: rpc_config.initial_writes_cache_size() as u64,
            latest_values_cache_size: rpc_config.latest_values_cache_size() as u64,
        };

        // On main node we always use master pool sink.
        self.node.add_layer(MasterPoolSinkLayer);
        self.node.add_layer(TxSenderLayer::new(
            TxSenderConfig::new(
                &sk_config,
                &rpc_config,
                try_load_config!(self.wallets.state_keeper)
                    .fee_account
                    .address(),
                self.genesis_config.l2_chain_id,
            ),
            postgres_storage_caches_config,
            rpc_config.vm_concurrency_limit(),
            ApiContracts::load_from_disk_blocking(), // TODO (BFT-138): Allow to dynamically reload API contracts
        ));
        Ok(self)
    }

    fn add_api_caches_layer(mut self) -> anyhow::Result<Self> {
        let rpc_config = try_load_config!(self.configs.api_config).web3_json_rpc;
        self.node.add_layer(MempoolCacheLayer::new(
            rpc_config.mempool_cache_size(),
            rpc_config.mempool_cache_update_interval(),
        ));
        Ok(self)
    }

    fn add_tree_api_client_layer(mut self) -> anyhow::Result<Self> {
        let rpc_config = try_load_config!(self.configs.api_config).web3_json_rpc;
        self.node
            .add_layer(TreeApiClientLayer::http(rpc_config.tree_api_url));
        Ok(self)
    }

    fn add_http_web3_api_layer(mut self) -> anyhow::Result<Self> {
        let rpc_config = try_load_config!(self.configs.api_config).web3_json_rpc;
        let state_keeper_config = try_load_config!(self.configs.state_keeper_config);
        let with_debug_namespace = state_keeper_config.save_call_traces;

        let mut namespaces = if let Some(namespaces) = &rpc_config.api_namespaces {
            namespaces
                .iter()
                .map(|a| a.parse())
                .collect::<Result<_, _>>()?
        } else {
            Namespace::DEFAULT.to_vec()
        };
        if with_debug_namespace {
            namespaces.push(Namespace::Debug)
        }
        namespaces.push(Namespace::Snapshots);

        let optional_config = Web3ServerOptionalConfig {
            namespaces: Some(namespaces),
            filters_limit: Some(rpc_config.filters_limit()),
            subscriptions_limit: Some(rpc_config.subscriptions_limit()),
            batch_request_size_limit: Some(rpc_config.max_batch_request_size()),
            response_body_size_limit: Some(rpc_config.max_response_body_size()),
            ..Default::default()
        };
        self.node.add_layer(Web3ServerLayer::http(
            rpc_config.http_port,
            InternalApiConfig::new(&rpc_config, &self.contracts_config, &self.genesis_config),
            optional_config,
        ));

        Ok(self)
    }

    fn add_vm_runner_protective_reads_layer(mut self) -> anyhow::Result<Self> {
        let protective_reads_writer_config: zksync_config::configs::ProtectiveReadsWriterConfig =
            try_load_config!(self.configs.protective_reads_writer_config);
        self.node.add_layer(ProtectiveReadsWriterLayer::new(
            protective_reads_writer_config,
            self.genesis_config.l2_chain_id,
        ));

        Ok(self)
    }

    fn add_vm_runner_bwip_layer(mut self) -> anyhow::Result<Self> {
        let basic_witness_input_producer_config =
            try_load_config!(self.configs.basic_witness_input_producer_config);
        self.node.add_layer(BasicWitnessInputProducerLayer::new(
            basic_witness_input_producer_config,
            self.genesis_config.l2_chain_id,
        ));

        Ok(self)
    }

    fn add_state_keeper_layer(mut self) -> anyhow::Result<Self> {
        // Bytecode compression is currently mandatory for the transactions processed by the sequencer.
        const OPTIONAL_BYTECODE_COMPRESSION: bool = false;

        let wallets = self.wallets.clone();
        let sk_config = try_load_config!(self.configs.state_keeper_config);
        let persistence_layer = OutputHandlerLayer::new(
            self.contracts_config
                .l2_shared_bridge_addr
                .context("L2 shared bridge address")?,
            sk_config.l2_block_seal_queue_capacity,
        )
        .with_protective_reads_persistence_enabled(sk_config.protective_reads_persistence_enabled);
        let mempool_io_layer = MempoolIOLayer::new(
            self.genesis_config.l2_chain_id,
            sk_config.clone(),
            try_load_config!(self.configs.mempool_config),
            try_load_config!(wallets.state_keeper),
        );
        let db_config = try_load_config!(self.configs.db_config);
        let experimental_vm_config = self
            .configs
            .experimental_vm_config
            .clone()
            .unwrap_or_default();
        let main_node_batch_executor_builder_layer =
            MainBatchExecutorLayer::new(sk_config.save_call_traces, OPTIONAL_BYTECODE_COMPRESSION)
                .with_fast_vm_mode(experimental_vm_config.state_keeper_fast_vm_mode);

        let rocksdb_options = RocksdbStorageOptions {
            block_cache_capacity: db_config
                .experimental
                .state_keeper_db_block_cache_capacity(),
            max_open_files: db_config.experimental.state_keeper_db_max_open_files,
        };
        let state_keeper_layer =
            StateKeeperLayer::new(db_config.state_keeper_db_path, rocksdb_options);
        self.node
            .add_layer(persistence_layer)
            .add_layer(mempool_io_layer)
            .add_layer(main_node_batch_executor_builder_layer)
            .add_layer(state_keeper_layer);
        Ok(self)
    }

    fn add_metadata_calculator_layer(mut self, with_tree_api: bool) -> anyhow::Result<Self> {
        let merkle_tree_env_config = try_load_config!(self.configs.db_config).merkle_tree;
        let operations_manager_env_config =
            try_load_config!(self.configs.operations_manager_config);
        let state_keeper_env_config = try_load_config!(self.configs.state_keeper_config);
        let metadata_calculator_config = MetadataCalculatorConfig::for_main_node(
            &merkle_tree_env_config,
            &operations_manager_env_config,
            &state_keeper_env_config,
        );
        let mut layer = MetadataCalculatorLayer::new(metadata_calculator_config);
        if with_tree_api {
            let merkle_tree_api_config = try_load_config!(self.configs.api_config).merkle_tree;
            layer = layer.with_tree_api_config(merkle_tree_api_config);
        }
        self.node.add_layer(layer);
        Ok(self)
    }

    fn add_logs_bloom_backfill_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(LogsBloomBackfillLayer);

        Ok(self)
    }

    fn add_commitment_generator_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(CommitmentGeneratorLayer::new(
            self.genesis_config.l1_batch_commit_data_generator_mode,
        ));

        Ok(self)
    }

    fn add_da_dispatcher_layer(mut self) -> anyhow::Result<Self> {
        let state_keeper_config = try_load_config!(self.configs.state_keeper_config);
        let da_config = try_load_config!(self.configs.da_dispatcher_config);
        let btc_sender_config = try_load_config!(self.configs.via_btc_sender_config);

        let dispatch_real_proof =
            btc_sender_config.proof_sending_mode != ProofSendingMode::SkipEveryProof;
        self.node.add_layer(DataAvailabilityDispatcherLayer::new(
            state_keeper_config,
            da_config,
            dispatch_real_proof,
        ));

        Ok(self)
    }

    fn add_via_celestia_da_client_layer(mut self) -> anyhow::Result<Self> {
        let celestia_config = try_load_config!(self.configs.via_celestia_config);
        self.node
            .add_layer(ViaCelestiaClientWiringLayer::new(celestia_config));
        Ok(self)
    }

    /// Builds the node with the genesis initialization task only.
    pub fn only_genesis(mut self) -> anyhow::Result<ZkStackService> {
        self = self
            .add_pools_layer()?
            .add_query_eth_client_layer()?
            .add_storage_initialization_layer(LayerKind::Task)?;

        Ok(self.node.build())
    }

    pub fn build(self) -> anyhow::Result<ZkStackService> {
        Ok(self
            .add_pools_layer()?
            .add_sigint_handler_layer()?
            .add_object_store_layer()?
            .add_healthcheck_layer()?
            .add_circuit_breaker_checker_layer()?
            .add_postgres_metrics_layer()?
            .add_query_eth_client_layer()?
            .add_prometheus_exporter_layer()?
            .add_storage_initialization_layer(LayerKind::Precondition)?
            // VIA layers
            .add_btc_watcher_layer()?
            .add_btc_sender_layer()?
            .add_l1_gas_layer()?
            .add_tx_sender_layer()?
            .add_api_caches_layer()?
            .add_tree_api_client_layer()?
            .add_http_web3_api_layer()?
            .add_vm_runner_protective_reads_layer()?
            .add_vm_runner_bwip_layer()?
            .add_storage_initialization_layer(LayerKind::Task)?
            .add_state_keeper_layer()?
            .add_logs_bloom_backfill_layer()?
            .add_metadata_calculator_layer(true)?
            .add_commitment_generator_layer()?
            // .add_via_celestia_da_client_layer()?
            // .add_da_dispatcher_layer()?
            .node
            .build())
    }
}

#[derive(Debug)]
enum LayerKind {
    Task,
    Precondition,
}
