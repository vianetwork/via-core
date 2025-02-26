use anyhow::Context;
use via_da_clients::celestia::wiring_layer::ViaCelestiaClientWiringLayer;
use zksync_config::{
    configs::{via_verifier::VerifierMode, Secrets},
    ActorRole, ViaGeneralConfig,
};
use zksync_node_framework::{
    implementations::layers::{
        circuit_breaker_checker::CircuitBreakerCheckerLayer,
        healtcheck_server::HealthCheckLayer,
        pools_layer::PoolsLayerBuilder,
        sigint::SigintHandlerLayer,
        via_btc_sender::{
            vote::ViaBtcVoteInscriptionLayer, vote_manager::ViaInscriptionManagerLayer,
        },
        via_verifier::{
            coordinator_api::ViaCoordinatorApiLayer, verifier::ViaWithdrawalVerifierLayer,
        },
        via_verifier_btc_watch::VerifierBtcWatchLayer,
        via_zk_verification::ViaBtcProofVerificationLayer,
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
    is_coordinator: bool,
    node: ZkStackServiceBuilder,
    configs: ViaGeneralConfig,
    secrets: Secrets,
}

impl ViaNodeBuilder {
    pub fn new(via_general_config: ViaGeneralConfig, secrets: Secrets) -> anyhow::Result<Self> {
        let via_verifier_config = try_load_config!(via_general_config.via_verifier_config);
        let is_coordinator = via_verifier_config.verifier_mode == VerifierMode::COORDINATOR;
        Ok(Self {
            is_coordinator,
            node: ZkStackServiceBuilder::new().context("Cannot create ZkStackServiceBuilder")?,
            configs: via_general_config,
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

    fn add_via_celestia_da_client_layer(mut self) -> anyhow::Result<Self> {
        let celestia_config = try_load_config!(self.configs.via_celestia_config);
        self.node
            .add_layer(ViaCelestiaClientWiringLayer::new(celestia_config));
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

    fn add_btc_sender_layer(mut self) -> anyhow::Result<Self> {
        let btc_sender_config = try_load_config!(self.configs.via_btc_sender_config);
        self.node
            .add_layer(ViaBtcVoteInscriptionLayer::new(btc_sender_config.clone()));
        self.node
            .add_layer(ViaInscriptionManagerLayer::new(btc_sender_config));
        Ok(self)
    }

    // VIA related layers
    fn add_verifier_btc_watcher_layer(mut self) -> anyhow::Result<Self> {
        let btc_watch_config = try_load_config!(self.configs.via_btc_watch_config);
        assert_eq!(
            btc_watch_config.actor_role,
            ActorRole::Verifier,
            "Verifier role is expected"
        );
        self.node
            .add_layer(VerifierBtcWatchLayer::new(btc_watch_config));
        Ok(self)
    }

    fn add_pools_layer(mut self) -> anyhow::Result<Self> {
        let config = try_load_config!(self.configs.postgres_config);
        let secrets = try_load_config!(self.secrets.database);
        let pools_layer = PoolsLayerBuilder::empty(config, secrets)
            .with_verifier(true)
            .build();
        self.node.add_layer(pools_layer);
        Ok(self)
    }

    fn add_verifier_coordinator_api_layer(mut self) -> anyhow::Result<Self> {
        let via_verifier_config = try_load_config!(self.configs.via_verifier_config);
        let via_btc_sender_config = try_load_config!(self.configs.via_btc_sender_config);
        self.node.add_layer(ViaCoordinatorApiLayer {
            config: via_verifier_config,
            btc_sender_config: via_btc_sender_config,
        });
        Ok(self)
    }

    fn add_withdrawal_verifier_task_layer(mut self) -> anyhow::Result<Self> {
        let via_verifier_config = try_load_config!(self.configs.via_verifier_config);
        let via_btc_sender_config = try_load_config!(self.configs.via_btc_sender_config);
        self.node.add_layer(ViaWithdrawalVerifierLayer {
            config: via_verifier_config,
            btc_sender_config: via_btc_sender_config,
        });
        Ok(self)
    }

    fn add_zkp_verification_layer(mut self) -> anyhow::Result<Self> {
        let via_verifier_config = try_load_config!(self.configs.via_verifier_config);
        let via_btc_watcher_config = try_load_config!(self.configs.via_btc_watch_config);
        self.node.add_layer(ViaBtcProofVerificationLayer {
            config: via_verifier_config,
            btc_watcher_config: via_btc_watcher_config,
        });
        Ok(self)
    }

    pub fn build(mut self) -> anyhow::Result<ZkStackService> {
        self = self
            .add_sigint_handler_layer()?
            .add_healthcheck_layer()?
            .add_circuit_breaker_checker_layer()?
            .add_pools_layer()?
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
