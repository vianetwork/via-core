use anyhow::Context;
use via_da_clients::celestia::wiring_layer::ViaCelestiaClientWiringLayer;
use zksync_config::{
    configs::{via_secrets::ViaSecrets, via_wallets::ViaWallets},
    GenesisConfig, ViaGeneralConfig,
};
use zksync_node_framework::{
    implementations::layers::{
        circuit_breaker_checker::CircuitBreakerCheckerLayer,
        healtcheck_server::HealthCheckLayer,
        pools_layer::PoolsLayerBuilder,
        prometheus_exporter::PrometheusExporterLayer,
        sigint::SigintHandlerLayer,
        via_btc_client::BtcClientLayer,
        via_btc_sender::{
            vote::ViaBtcVoteInscriptionLayer, vote_manager::ViaInscriptionManagerLayer,
        },
        via_verifier::{
            coordinator_api::ViaCoordinatorApiLayer, verifier::ViaWithdrawalVerifierLayer,
        },
        via_verifier_btc_watch::VerifierBtcWatchLayer,
        via_verifier_storage_init::ViaVerifierInitLayer,
        via_zk_verification::ViaBtcProofVerificationLayer,
    },
    service::{ZkStackService, ZkStackServiceBuilder},
};
use zksync_types::via_roles::ViaNodeRole;
use zksync_vlog::prometheus::PrometheusExporterConfig;

/// Macro that looks into a path to fetch an optional config,
/// and clones it into a variable.
macro_rules! try_load_config {
    ($path:expr) => {
        $path.as_ref().context(stringify!($path))?.clone()
    };
}

pub struct ViaNodeBuilder {
    is_coordinator: bool,
    node: ZkStackServiceBuilder,
    configs: ViaGeneralConfig,
    genesis_config: GenesisConfig,
    secrets: ViaSecrets,
    wallets: ViaWallets,
}

impl ViaNodeBuilder {
    pub fn new(
        via_general_config: ViaGeneralConfig,
        genesis_config: GenesisConfig,
        secrets: ViaSecrets,
        wallets: ViaWallets,
    ) -> anyhow::Result<Self> {
        let via_verifier_config = try_load_config!(via_general_config.via_verifier_config);
        let is_coordinator = via_verifier_config.role == ViaNodeRole::Coordinator;
        Ok(Self {
            is_coordinator,
            node: ZkStackServiceBuilder::new().context("Cannot create ZkStackServiceBuilder")?,
            configs: via_general_config,
            genesis_config,
            secrets,
            wallets,
        })
    }

    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.node.runtime_handle()
    }

    fn add_sigint_handler_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(SigintHandlerLayer);
        Ok(self)
    }

    fn add_btc_client_layer(mut self) -> anyhow::Result<Self> {
        let via_btc_client_config = try_load_config!(self.configs.via_btc_client_config);
        let secrets = self.secrets.via_l1.clone().unwrap();

        self.node
            .add_layer(BtcClientLayer::new(via_btc_client_config, secrets));
        Ok(self)
    }

    fn add_via_celestia_da_client_layer(mut self) -> anyhow::Result<Self> {
        let secrets = self.secrets.via_da.clone().unwrap();
        let celestia_config = try_load_config!(self.configs.via_celestia_config);
        self.node
            .add_layer(ViaCelestiaClientWiringLayer::new(celestia_config, secrets));
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

    fn add_prometheus_exporter_layer(mut self) -> anyhow::Result<Self> {
        let prom_config = try_load_config!(self.configs.prometheus_config);
        let prom_config = PrometheusExporterConfig::pull(prom_config.listener_port);
        self.node.add_layer(PrometheusExporterLayer(prom_config));
        Ok(self)
    }

    fn add_btc_sender_layer(mut self) -> anyhow::Result<Self> {
        let wallet = self
            .wallets
            .btc_sender
            .clone()
            .expect("Empty btc sender wallet");

        let btc_sender_config = try_load_config!(self.configs.via_btc_sender_config);
        self.node
            .add_layer(ViaBtcVoteInscriptionLayer::new(btc_sender_config.clone()));
        self.node
            .add_layer(ViaInscriptionManagerLayer::new(btc_sender_config, wallet));
        Ok(self)
    }

    // VIA related layers
    fn add_verifier_btc_watcher_layer(mut self) -> anyhow::Result<Self> {
        let via_genesis_config = try_load_config!(self.configs.via_genesis_config);
        let via_btc_client_config = try_load_config!(self.configs.via_btc_client_config);
        let via_btc_watch_config = try_load_config!(self.configs.via_btc_watch_config);

        self.node.add_layer(VerifierBtcWatchLayer::new(
            via_genesis_config,
            via_btc_client_config,
            via_btc_watch_config,
        ));
        Ok(self)
    }

    fn add_pools_layer(mut self) -> anyhow::Result<Self> {
        let config = try_load_config!(self.configs.postgres_config);
        let secrets = try_load_config!(self.secrets.base_secrets.database);
        let pools_layer = PoolsLayerBuilder::empty(config, secrets)
            .with_verifier(true)
            .build();
        self.node.add_layer(pools_layer);
        Ok(self)
    }

    fn add_verifier_coordinator_api_layer(mut self) -> anyhow::Result<Self> {
        let via_genesis_config = try_load_config!(self.configs.via_genesis_config);
        let via_btc_client_config = try_load_config!(self.configs.via_btc_client_config);
        let via_verifier_config = try_load_config!(self.configs.via_verifier_config);

        self.node.add_layer(ViaCoordinatorApiLayer::new(
            via_genesis_config,
            via_btc_client_config,
            via_verifier_config,
        ));
        Ok(self)
    }

    fn add_withdrawal_verifier_task_layer(mut self) -> anyhow::Result<Self> {
        let via_genesis_config = try_load_config!(self.configs.via_genesis_config);
        let via_btc_client_config = try_load_config!(self.configs.via_btc_client_config);
        let via_verifier_config = try_load_config!(self.configs.via_verifier_config);
        let wallet = self
            .wallets
            .vote_operator
            .clone()
            .expect("Empty verifier wallet");

        self.node.add_layer(ViaWithdrawalVerifierLayer::new(
            via_genesis_config,
            via_btc_client_config,
            via_verifier_config,
            wallet,
        ));
        Ok(self)
    }

    fn add_zkp_verification_layer(mut self) -> anyhow::Result<Self> {
        let via_verifier_config = try_load_config!(self.configs.via_verifier_config);
        let via_genesis_config = try_load_config!(self.configs.via_genesis_config);
        self.node.add_layer(ViaBtcProofVerificationLayer::new(
            via_verifier_config,
            via_genesis_config,
        ));
        Ok(self)
    }

    fn add_storage_initialization_layer(mut self) -> anyhow::Result<Self> {
        let layer = ViaVerifierInitLayer {
            genesis: self.genesis_config.clone(),
        };
        self.node.add_layer(layer);
        Ok(self)
    }

    pub fn build(mut self) -> anyhow::Result<ZkStackService> {
        self = self
            .add_sigint_handler_layer()?
            .add_healthcheck_layer()?
            .add_circuit_breaker_checker_layer()?
            .add_prometheus_exporter_layer()?
            .add_pools_layer()?
            .add_storage_initialization_layer()?
            .add_btc_client_layer()?
            .add_btc_sender_layer()?
            .add_verifier_btc_watcher_layer()?
            .add_via_celestia_da_client_layer()?
            .add_zkp_verification_layer()?;

        if self.is_coordinator {
            self = self.add_verifier_coordinator_api_layer()?
        }

        self = self.add_withdrawal_verifier_task_layer()?;

        Ok(self.node.build())
    }
}
