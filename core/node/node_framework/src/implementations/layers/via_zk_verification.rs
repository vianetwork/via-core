use async_trait::async_trait;
use via_zk_verifier::ViaVerifier;
use zksync_config::{ViaBtcWatchConfig, ViaVerifierConfig};

use crate::{
    implementations::resources::{
        da_client::DAClientResource,
        object_store::ObjectStoreResource,
        pools::{PoolResource, VerifierPool},
        via_btc_client::BtcClientResource,
        via_btc_indexer::BtcIndexerResource,
    },
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

#[derive(Debug)]
pub struct ViaBtcProofVerificationLayer {
    verifier_config: ViaVerifierConfig,
    btc_watch_config: ViaBtcWatchConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct ProofVerificationInput {
    pub master_pool: PoolResource<VerifierPool>,
    pub da_client: DAClientResource,
    pub btc_client_resource: BtcClientResource,
    pub btc_indexer_resource: BtcIndexerResource,
    pub object_store: ObjectStoreResource,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct ProofVerificationOutput {
    #[context(task)]
    pub via_proof_verification: ViaVerifier,
}

impl ViaBtcProofVerificationLayer {
    pub fn new(verifier_config: ViaVerifierConfig, btc_watch_config: ViaBtcWatchConfig) -> Self {
        Self {
            verifier_config,
            btc_watch_config,
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
        let blob_store = input.object_store.0;

        let btc_client = input.btc_client_resource.verifier.unwrap();

        let via_proof_verification = ViaVerifier::new(
            self.verifier_config,
            input.btc_indexer_resource.0.as_ref().clone(),
            main_pool,
            input.da_client.0,
            btc_client,
            self.btc_watch_config,
            blob_store,
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
