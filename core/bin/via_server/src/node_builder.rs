use anyhow::Context;
use zksync_config::{
    configs::{wallets::Wallets, GeneralConfig, PostgresConfig, Secrets},
    ContractsConfig, GenesisConfig, ViaBtcWatchConfig, ViaGeneralConfig,
};
use zksync_node_api_server::{
    tx_sender::{ApiContracts, TxSenderConfig},
    web3::{state::InternalApiConfig, Namespace},
};
use zksync_node_framework::{
    implementations::layers::{
        circuit_breaker_checker::CircuitBreakerCheckerLayer,
        healtcheck_server::HealthCheckLayer,
        house_keeper::HouseKeeperLayer,
        object_store::ObjectStoreLayer,
        pools_layer::PoolsLayerBuilder,
        postgres_metrics::PostgresMetricsLayer,
        prometheus_exporter::PrometheusExporterLayer,
        sigint::SigintHandlerLayer,
        via_btc_sender::{
            aggregator::ViaBtcInscriptionAggregatorLayer, manager::ViaInscriptionManagerLayer,
        },
        via_btc_watch::BtcWatchLayer,
        via_l1_gas::ViaL1GasLayer,
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

    fn add_circuit_breaker_checker_layer(mut self) -> anyhow::Result<Self> {
        let circuit_breaker_config = try_load_config!(self.configs.circuit_breaker_config);
        self.node
            .add_layer(CircuitBreakerCheckerLayer(circuit_breaker_config));
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

    pub fn build(self) -> anyhow::Result<ZkStackService> {
        Ok(self
            .add_pools_layer()?
            .add_sigint_handler_layer()?
            .add_object_store_layer()?
            .add_healthcheck_layer()?
            .add_circuit_breaker_checker_layer()?
            .add_postgres_metrics_layer()?
            // VIA layers
            .add_btc_watcher_layer()?
            .add_btc_sender_layer()?
            .add_l1_gas_layer()?
            .add_tx_sender_layer()?
            .add_api_caches_layer()?
            .add_tree_api_client_layer()?
            .add_http_web3_api_layer()?
            .node
            .build())
    }
}
