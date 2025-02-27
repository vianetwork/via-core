use via_verifier_btc_sender::btc_vote_inscription::ViaVoteInscription;
use zksync_config::ViaBtcSenderConfig;

use crate::{
    implementations::resources::pools::{PoolResource, VerifierPool},
    service::StopReceiver,
    task::{Task, TaskId},
    wiring_layer::{WiringError, WiringLayer},
    FromContext, IntoContext,
};

/// Wiring layer for btc_sender vote inscription
///
/// Responsible for initialization and running of [`ViaVoteInscription`], that create `Vote` inscription requests
#[derive(Debug)]
pub struct ViaBtcVoteInscriptionLayer {
    config: ViaBtcSenderConfig,
}

#[derive(Debug, FromContext)]
#[context(crate = crate)]
pub struct Input {
    pub master_pool: PoolResource<VerifierPool>,
}

#[derive(Debug, IntoContext)]
#[context(crate = crate)]
pub struct Output {
    #[context(task)]
    pub via_vote_inscription: ViaVoteInscription,
}

impl ViaBtcVoteInscriptionLayer {
    pub fn new(config: ViaBtcSenderConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl WiringLayer for ViaBtcVoteInscriptionLayer {
    type Input = Input;
    type Output = Output;

    fn layer_name(&self) -> &'static str {
        "via_btc_verifier_vote_inscription_layer"
    }

    async fn wire(self, input: Self::Input) -> Result<Self::Output, WiringError> {
        // Get resources.
        let master_pool = input.master_pool.get().await.unwrap();

        let via_vote_inscription = ViaVoteInscription::new(master_pool, self.config).await?;

        Ok(Output {
            via_vote_inscription,
        })
    }
}

#[async_trait::async_trait]
impl Task for ViaVoteInscription {
    fn id(&self) -> TaskId {
        "via_vote_inscription".into()
    }

    async fn run(self: Box<Self>, stop_receiver: StopReceiver) -> anyhow::Result<()> {
        (*self).run(stop_receiver.0).await
    }
}
