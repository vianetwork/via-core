use std::str::FromStr;

use anyhow::Context;
use via_btc_client::{
    types::{BitcoinAddress, NodeAuth},
    withdrawal_builder::WithdrawalBuilder,
};
use via_btc_watch::BitcoinNetwork;
use via_verifier_dal::{ConnectionPool, Verifier};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::{ViaBtcSenderConfig, ViaVerifierConfig};

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
    pub config: ViaVerifierConfig,
    pub btc_sender_config: ViaBtcSenderConfig,
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

#[async_trait::async_trait]
impl WiringLayer for ViaCoordinatorApiLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_coordinator_api_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let master_pool = input.master_pool.get().await?;
        let auth = NodeAuth::UserPass(
            self.btc_sender_config.rpc_user().to_string(),
            self.btc_sender_config.rpc_password().to_string(),
        );
        let network = BitcoinNetwork::from_str(self.btc_sender_config.network()).unwrap();
        let bridge_address = BitcoinAddress::from_str(self.config.bridge_address_str.as_str())
            .context("Error parse bridge address")?
            .assume_checked();

        let withdrawal_builder = WithdrawalBuilder::new(
            self.btc_sender_config.rpc_url(),
            network,
            auth,
            bridge_address,
        )
        .await?;

        let withdrawal_client = WithdrawalClient::new(input.client.0, network);
        let via_coordinator_api_task = ViaCoordinatorApiTask {
            master_pool,
            config: self.config,
            withdrawal_builder,
            withdrawal_client,
        };
        Ok(Output {
            via_coordinator_api_task,
        })
    }
}

#[derive(Debug)]
pub struct ViaCoordinatorApiTask {
    master_pool: ConnectionPool<Verifier>,
    config: ViaVerifierConfig,
    withdrawal_builder: WithdrawalBuilder,
    withdrawal_client: WithdrawalClient,
}

#[async_trait::async_trait]
impl Task for ViaCoordinatorApiTask {
    fn id(&self) -> TaskId {
        "via_coordinator_api".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        via_withdrawal_service::coordinator::api::start_coordinator_server(
            self.config,
            self.master_pool,
            self.withdrawal_builder,
            self.withdrawal_client,
            stop_receiver.0,
        )
        .await
    }
}
