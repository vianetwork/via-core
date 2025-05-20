use anyhow::Context;
use via_verifier_coordinator::verifier::ViaWithdrawalVerifier;
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::{
    configs::{
        via_btc_client::ViaBtcClientConfig, via_consensus::ViaGenesisConfig, via_wallets::ViaWallet,
    },
    ViaVerifierConfig,
};

use crate::{
    implementations::resources::{
        da_client::DAClientResource,
        pools::{PoolResource, VerifierPool},
        via_btc_client::BtcClientResource,
    },
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for verifier task
#[derive(Debug)]
pub struct ViaWithdrawalVerifierLayer {
    via_genesis_config: ViaGenesisConfig,
    via_btc_client: ViaBtcClientConfig,
    verifier_config: ViaVerifierConfig,
    wallet: ViaWallet,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<VerifierPool>,
    pub da_client: DAClientResource,
    pub btc_client_resource: BtcClientResource,
}

#[derive(IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub via_withdrawal_verifier_task: ViaWithdrawalVerifier,
}

impl ViaWithdrawalVerifierLayer {
    pub fn new(
        via_genesis_config: ViaGenesisConfig,
        via_btc_client: ViaBtcClientConfig,
        verifier_config: ViaVerifierConfig,
        wallet: ViaWallet,
    ) -> Self {
        Self {
            via_genesis_config,
            via_btc_client,
            verifier_config,
            wallet,
        }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaWithdrawalVerifierLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_withdrawal_verifier_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let master_pool = input.master_pool.get().await?;

        let withdrawal_client =
            WithdrawalClient::new(input.da_client.0, self.via_btc_client.network());

        let btc_client = input.btc_client_resource.0;

        let via_withdrawal_verifier_task = ViaWithdrawalVerifier::new(
            self.verifier_config,
            self.wallet,
            master_pool,
            btc_client,
            withdrawal_client,
            self.via_genesis_config.bridge_address()?,
            self.via_genesis_config.verifiers_pub_keys,
        )
        .context("Error to init the via withdrawal verifier")?;

        Ok(Output {
            via_withdrawal_verifier_task,
        })
    }
}

#[async_trait::async_trait]
impl Task for ViaWithdrawalVerifier {
    fn id(&self) -> TaskId {
        "via_withdrawal_verifier".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
