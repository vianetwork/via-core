use async_trait::async_trait;
use via_btc_client::types::{BitcoinNetwork, NodeAuth};
use via_zk_verifier::ViaVerifier;
use zksync_config::{ViaBtcWatchConfig, ViaVerifierConfig};

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

#[derive(Debug)]
pub struct ViaBtcProofVerificationLayer {
    pub config: ViaVerifierConfig,
    pub btc_watcher_config: ViaBtcWatchConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct ProofVerificationInput {
    pub master_pool: PoolResource<VerifierPool>,
    pub da_client: DAClientResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct ProofVerificationOutput {
    #[context(task)]
    pub via_proof_verification: ViaVerifier,
}

impl ViaBtcProofVerificationLayer {
    pub fn new(config: ViaVerifierConfig, btc_watcher_config: ViaBtcWatchConfig) -> Self {
        Self {
            config,
            btc_watcher_config,
        }
    }
}

#[async_trait]
impl WiringLayer for ViaBtcProofVerificationLayer {
    type Input = ProofVerificationInput;
    type Output = ProofVerificationOutput;

    fn layer_name(&self) -> &'static str {
        "via_btc_proof_verification_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        let main_pool = input.master_pool.get().await?;
        let network = BitcoinNetwork::from_core_arg(self.btc_watcher_config.network())
            .map_err(|_| WiringError::Configuration("Wrong network in config".to_string()))?;
        let node_auth = NodeAuth::UserPass(
            self.btc_watcher_config.rpc_user().to_string(),
            self.btc_watcher_config.rpc_password().to_string(),
        );
        let bootstrap_txids = self
            .btc_watcher_config
            .bootstrap_txids()
            .iter()
            .map(|txid| {
                txid.parse()
                    .map_err(|_| WiringError::Configuration("Wrong txid in config".to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let via_proof_verification = ViaVerifier::new(
            self.btc_watcher_config.rpc_url(),
            network,
            node_auth,
            bootstrap_txids,
            main_pool,
            input.da_client.0,
            self.config.clone(),
        )
        .await
        .map_err(WiringError::internal)?;

        Ok(ProofVerificationOutput {
            via_proof_verification,
        })
    }
}

#[async_trait::async_trait]
impl Task for ViaVerifier {
    fn id(&self) -> TaskId {
        "via_proof_verification".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
