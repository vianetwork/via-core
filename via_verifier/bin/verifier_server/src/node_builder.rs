use anyhow::Context;
use zksync_config::{
    configs::{wallets::Wallets, Secrets},
    ContractsConfig, GenesisConfig, ViaGeneralConfig,
};
use zksync_node_framework::{
    implementations::layers::{
        circuit_breaker_checker::CircuitBreakerCheckerLayer, healtcheck_server::HealthCheckLayer,
        sigint::SigintHandlerLayer, via_verifier_withdrawal::coordinator::ViaCoordinatorApiLayer,
    },
    service::{ZkStackService, ZkStackServiceBuilder},
};

/// Macro that looks into a path to fetch an optional config,
/// and clones it into a variable.
macro_rules! try_load_config {
    ($path:expr) => {
        $path.as_ref().context(stringify!($path))?.clone()
    };
}

pub struct ViaNodeBuilder {
    coordinator: bool,
    node: ZkStackServiceBuilder,
    configs: ViaGeneralConfig,
    wallets: Wallets,
    genesis_config: GenesisConfig,
    contracts_config: ContractsConfig,
    secrets: Secrets,
}

impl ViaNodeBuilder {
    pub fn new(
        coordinator: bool,
        via_general_config: ViaGeneralConfig,
        wallets: Wallets,
        secrets: Secrets,
        genesis_config: GenesisConfig,
        contracts_config: ContractsConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            coordinator,
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

    fn add_pools_layer(mut self) -> anyhow::Result<Self> {
        let config = try_load_config!(self.configs.postgres_config);
        let secrets = try_load_config!(self.secrets.database);
        let pools_layer = PoolsLayerBuilder::empty(config, secrets)
            .with_master(false)
            .with_replica(false)
            .with_prover(false) // Used by house keeper.
            .with_via_verifier(true)
            .build();
        self.node.add_layer(pools_layer);
        Ok(self)
    }

    fn add_postgres_metrics_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(PostgresMetricsLayer);
        fn add_coordinator_api_layer(mut self) -> anyhow::Result<Self> {
            // self.node.add_layer(ViaCoordinatorApiLayer);
            Ok(self)
        }

        pub fn build(self) -> anyhow::Result<ZkStackService> {
            Ok(self
                .add_sigint_handler_layer()?
                .add_healthcheck_layer()?
                .add_circuit_breaker_checker_layer()?
                .add_pools_layer()?
                .add_postgres_metrics_layer()?
                .add_coordinator_api_layer()?
                .node
                .build())
        }
    }
}
