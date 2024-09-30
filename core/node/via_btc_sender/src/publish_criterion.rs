use std::fmt;

use async_trait::async_trait;
use chrono::Utc;
use zksync_dal::{Connection, Core};
use zksync_types::{commitment::L1BatchWithMetadata, L1BatchNumber};

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
pub struct ViaNumberCriterion {}

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
        consecutive_l1_batches
            .first()
            .map(|batch| batch.header.number)
    }
}

#[derive(Debug)]
pub struct TimestampDeadlineCriterion {
    /// Maximum L1 batch age in seconds. Once reached, we pack and publish all the available L1 batches.
    pub deadline_seconds: u32,
}

#[async_trait]
impl ViaBtcL1BatchCommitCriterion for TimestampDeadlineCriterion {
    fn name(&self) -> &'static str {
        "timestamp"
    }

    async fn last_l1_batch_to_publish(
        &mut self,
        _storage: &mut Connection<'_, Core>,
        consecutive_l1_batches: &[L1BatchWithMetadata],
    ) -> Option<L1BatchNumber> {
        let current_timestamp = Utc::now().timestamp() as u64;
        let block_timestamp = consecutive_l1_batches.first()?.header.timestamp;
        if block_timestamp + self.deadline_seconds as u64 <= current_timestamp {
            return Some(consecutive_l1_batches[0].header.number);
        }
        None
    }
}
