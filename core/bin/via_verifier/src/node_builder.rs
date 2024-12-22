use anyhow::Context;
use zksync_config::{
    configs::{wallets::Wallets, Secrets},
    ActorRole, ContractsConfig, GenesisConfig, ViaGeneralConfig,
};
use zksync_node_framework::{
    implementations::layers::{
        circuit_breaker_checker::CircuitBreakerCheckerLayer, healtcheck_server::HealthCheckLayer,
        sigint::SigintHandlerLayer, via_btc_watch::BtcWatchLayer,
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

    // VIA related layers
    fn add_verifier_btc_watcher_layer(mut self) -> anyhow::Result<Self> {
        let mut btc_watch_config = try_load_config!(self.configs.via_btc_watch_config);
        assert_eq!(
            btc_watch_config.actor_role,
            ActorRole::Verifier,
            "Verifier role is expected"
        );
        self.node.add_layer(BtcWatchLayer::new(btc_watch_config));
        Ok(self)
    }

    pub fn build(self) -> anyhow::Result<ZkStackService> {
        Ok(self
            .add_sigint_handler_layer()?
            .add_healthcheck_layer()?
            .add_circuit_breaker_checker_layer()?
            .add_verifier_btc_watcher_layer()?
            .node
            .build())
    }
}
