use anyhow::Context;
use via_da_clients::wiring_layer::ViaDaClientWiringLayer;
use zksync_config::configs::via_verifier::ViaGeneralVerifierConfig;
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
        via_verifier_block_reverter::VerifierBlockReverterLayer,
        via_verifier_btc_watch::VerifierBtcWatchLayer,
        via_verifier_reorg_detector::ViaVerifierReorgDetectorLayer,
        via_verifier_storage_init::ViaVerifierInitLayer,
        via_zk_verification::ViaBtcProofVerificationLayer,
    },
    service::{ZkStackService, ZkStackServiceBuilder},
};
use zksync_types::via_roles::ViaNodeRole;
use zksync_vlog::prometheus::PrometheusExporterConfig;

pub struct ViaNodeBuilder {
    is_coordinator: bool,
    node: ZkStackServiceBuilder,
    configs: ViaGeneralVerifierConfig,
}

impl ViaNodeBuilder {
    pub fn new(configs: ViaGeneralVerifierConfig) -> anyhow::Result<Self> {
        Ok(Self {
            is_coordinator: configs.via_verifier_config.role == ViaNodeRole::Coordinator,
            node: ZkStackServiceBuilder::new().context("Cannot create ZkStackServiceBuilder")?,
            configs,
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
        self.node.add_layer(BtcClientLayer::new(
            self.configs.via_btc_client_config.clone(),
            self.configs.secrets.via_l1.clone().unwrap(),
            self.configs.wallets.clone(),
            Some(self.configs.via_bridge_config.bridge_address.clone()),
        ));
        Ok(self)
    }

    fn add_via_da_client_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(ViaDaClientWiringLayer::new(
            self.configs.via_celestia_config.clone(),
            self.configs.secrets.via_da.clone(),
        ));
        Ok(self)
    }

    fn add_healthcheck_layer(mut self) -> anyhow::Result<Self> {
        self.node
            .add_layer(HealthCheckLayer(self.configs.health_check.clone()));
        Ok(self)
    }

    fn add_circuit_breaker_checker_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(CircuitBreakerCheckerLayer(
            self.configs.circuit_breaker_config.clone(),
        ));
        Ok(self)
    }

    fn add_prometheus_exporter_layer(mut self) -> anyhow::Result<Self> {
        let prom_config =
            PrometheusExporterConfig::pull(self.configs.prometheus_config.listener_port);
        self.node.add_layer(PrometheusExporterLayer(prom_config));
        Ok(self)
    }

    fn add_btc_sender_layer(mut self) -> anyhow::Result<Self> {
        let wallet = self
            .configs
            .wallets
            .btc_sender
            .clone()
            .expect("Empty btc sender wallet");

        self.node.add_layer(ViaBtcVoteInscriptionLayer::new(
            self.configs.via_btc_sender_config.clone(),
        ));
        self.node.add_layer(ViaInscriptionManagerLayer::new(
            self.configs.via_btc_sender_config.clone(),
            wallet,
        ));
        Ok(self)
    }

    // VIA related layers
    fn add_btc_watcher_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(VerifierBtcWatchLayer {
            via_bridge_config: self.configs.via_bridge_config.clone(),
            via_btc_watch_config: self.configs.via_btc_watch_config.clone(),
        });
        Ok(self)
    }

    fn add_pools_layer(mut self) -> anyhow::Result<Self> {
        let pools_layer = PoolsLayerBuilder::empty(
            self.configs.postgres_config.clone(),
            self.configs.secrets.base_secrets.database.clone().unwrap(),
        )
        .with_verifier(true)
        .build();
        self.node.add_layer(pools_layer);
        Ok(self)
    }

    fn add_verifier_coordinator_api_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(ViaCoordinatorApiLayer::new(
            self.configs.via_bridge_config.clone(),
            self.configs.via_btc_client_config.clone(),
            self.configs.via_verifier_config.clone(),
        ));
        Ok(self)
    }

    fn add_withdrawal_verifier_task_layer(mut self) -> anyhow::Result<Self> {
        let wallet = self
            .configs
            .wallets
            .vote_operator
            .clone()
            .expect("Empty verifier wallet");

        self.node.add_layer(ViaWithdrawalVerifierLayer::new(
            self.configs.via_bridge_config.clone(),
            self.configs.via_btc_client_config.clone(),
            self.configs.via_verifier_config.clone(),
            wallet,
        ));
        Ok(self)
    }

    fn add_zkp_verification_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(ViaBtcProofVerificationLayer::new(
            self.configs.via_verifier_config.clone(),
            self.configs.via_bridge_config.clone(),
        ));
        Ok(self)
    }

    fn add_block_reverter_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(VerifierBlockReverterLayer::new(
            self.configs.via_reorg_detector_config.clone(),
        ));
        Ok(self)
    }

    fn add_storage_initialization_layer(mut self) -> anyhow::Result<Self> {
        let layer = ViaVerifierInitLayer {
            genesis: self.configs.genesis_config.clone(),
            via_genesis_config: self.configs.via_genesis_config.clone(),
            via_btc_watch_config: self.configs.via_btc_watch_config.clone(),
        };
        self.node.add_layer(layer);
        Ok(self)
    }

    fn add_reorg_detector_layer(mut self) -> anyhow::Result<Self> {
        self.node.add_layer(ViaVerifierReorgDetectorLayer::new(
            self.configs.via_reorg_detector_config.clone(),
        ));
        Ok(self)
    }

    pub fn build(mut self) -> anyhow::Result<ZkStackService> {
        self = self
            .add_sigint_handler_layer()?
            .add_healthcheck_layer()?
            .add_circuit_breaker_checker_layer()?
            .add_prometheus_exporter_layer()?
            .add_pools_layer()?
            .add_block_reverter_layer()?
            .add_btc_client_layer()?
            .add_reorg_detector_layer()?
            .add_storage_initialization_layer()?
            .add_btc_sender_layer()?
            .add_btc_watcher_layer()?
            .add_via_da_client_layer()?
            .add_zkp_verification_layer()?;

        if self.is_coordinator {
            self = self.add_verifier_coordinator_api_layer()?
        }

        self = self.add_withdrawal_verifier_task_layer()?;

        Ok(self.node.build())
    }
}
