use std::sync::Arc;

use via_btc_client::{client::BitcoinClient, traits::BitcoinOps};
use via_verifier_dal::{ConnectionPool, Verifier};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::{
    configs::{
        via_btc_client::ViaBtcClientConfig, via_consensus::ViaGenesisConfig,
        via_secrets::ViaL1Secrets,
    },
    ViaVerifierConfig,
};

use crate::{
    implementations::resources::{
        da_client::DAClientResource,
        pools::{PoolResource, VerifierPool},
    },
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for coordinator api
#[derive(Debug)]
pub struct ViaCoordinatorApiLayer {
    via_genesis_config: ViaGenesisConfig,
    via_btc_client: ViaBtcClientConfig,
    verifier_config: ViaVerifierConfig,
    secrets: ViaL1Secrets,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<VerifierPool>,
    pub client: DAClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub via_coordinator_api_task: ViaCoordinatorApiTask,
}

impl ViaCoordinatorApiLayer {
    pub fn new(
        via_genesis_config: ViaGenesisConfig,
        via_btc_client: ViaBtcClientConfig,
        verifier_config: ViaVerifierConfig,
        secrets: ViaL1Secrets,
    ) -> Self {
        Self {
            via_genesis_config,
            via_btc_client,
            verifier_config,
            secrets,
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaCoordinatorApiLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_coordinator_api_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let master_pool = input.master_pool.get().await?;

        let btc_client = Arc::new(
            BitcoinClient::new(
                &format!(
                    "{}/wallet/{}",
                    self.secrets.rpc_url.expose_str(),
                    self.via_genesis_config.bridge_address()?.to_string()
                ),
                self.via_btc_client.network(),
                self.secrets.auth_node(),
            )
            .unwrap(),
        );

        let withdrawal_client =
            WithdrawalClient::new(input.client.0, self.via_btc_client.network());

        let via_coordinator_api_task = ViaCoordinatorApiTask {
            verifier_config: self.verifier_config,
            master_pool,
            btc_client,
            withdrawal_client,
            via_genesis_config: self.via_genesis_config,
        };
        Ok(Output {
            via_coordinator_api_task,
        })
    }
}

#[derive(Debug)]
pub struct ViaCoordinatorApiTask {
    verifier_config: ViaVerifierConfig,
    master_pool: ConnectionPool<Verifier>,
    btc_client: Arc<dyn BitcoinOps>,
    withdrawal_client: WithdrawalClient,
    via_genesis_config: ViaGenesisConfig,
}

#[async_trait::async_trait]
impl Task for ViaCoordinatorApiTask {
    fn id(&self) -> TaskId {
        "via_coordinator_api".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        via_verifier_coordinator::coordinator::api::start_coordinator_server(
            self.verifier_config,
            self.master_pool,
            self.btc_client,
            self.withdrawal_client,
            self.via_genesis_config.bridge_address()?,
            self.via_genesis_config.verifiers_pub_keys.clone(),
            self.via_genesis_config.required_signers,
            stop_receiver.0,
        )
        .await
    }
}
