use async_trait::async_trait;
use std::fmt;
use zksync_dal::{Connection, Core};
use zksync_types::{
    btc_inscription_operations::ViaBtcInscriptionRequestType, commitment::L1BatchWithMetadata,
    L1BatchNumber,
};

#[async_trait]
pub trait ViaBtcL1BatchCommitCriterion: fmt::Debug + Send + Sync {
    #[allow(dead_code)]
    // Takes `&self` receiver for the trait to be object-safe
    fn name(&self) -> &'static str;

    /// Returns `None` if there is no need to publish any L1 batches.
    /// Otherwise, returns the number of the last L1 batch that needs to be committed.
    async fn last_l1_batch_to_publish(
        &mut self,
        storage: &mut Connection<'_, Core>,
        consecutive_l1_batches: &[L1BatchWithMetadata],
    ) -> Option<L1BatchNumber>;
}

#[derive(Debug)]
pub struct ViaNumberCriterion {
    pub op: ViaBtcInscriptionRequestType,
    pub limit: i32,
}

#[async_trait]
impl ViaBtcL1BatchCommitCriterion for ViaNumberCriterion {
    fn name(&self) -> &'static str {
        "l1_batch_number"
    }

    async fn last_l1_batch_to_publish(
        &mut self,
        _storage: &mut Connection<'_, Core>,
        consecutive_l1_batches: &[L1BatchWithMetadata],
    ) -> Option<L1BatchNumber> {
        Some(1.into())
    }
}
