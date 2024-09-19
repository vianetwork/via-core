use std::fmt;

use async_trait::async_trait;
use chrono::Utc;
use zksync_types::{btc_block::ViaBtcL1BlockDetails, L1BatchNumber};

#[async_trait]
pub trait ViaBtcL1BatchCommitCriterion: fmt::Debug + Send + Sync {
    #[allow(dead_code)]
    // Takes `&self` receiver for the trait to be object-safe
    fn name(&self) -> &'static str;

    /// Returns `None` if there is no need to publish any L1 batches.
    /// Otherwise, returns the number of the last L1 batch that needs to be committed.
    async fn last_l1_batch_to_publish(
        &mut self,
        consecutive_l1_batches: &[ViaBtcL1BlockDetails],
    ) -> Option<L1BatchNumber>;
}

#[derive(Debug)]
pub struct ViaNumberCriterion {
    pub limit: u32,
}

#[async_trait]
impl ViaBtcL1BatchCommitCriterion for ViaNumberCriterion {
    fn name(&self) -> &'static str {
        "l1_batch_number"
    }

    async fn last_l1_batch_to_publish(
        &mut self,
        consecutive_l1_batches: &[ViaBtcL1BlockDetails],
    ) -> Option<L1BatchNumber> {
        let mut batch_numbers = consecutive_l1_batches.iter().map(|batch| batch.number.0);

        let first = batch_numbers.next()?;
        let last_batch_number = batch_numbers.last().unwrap_or(first);
        let batch_count = last_batch_number - first + 1;
        if batch_count >= self.limit {
            let result = L1BatchNumber(first + self.limit - 1);
            Some(result)
        } else {
            None
        }
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
        consecutive_l1_batches: &[ViaBtcL1BlockDetails],
    ) -> Option<L1BatchNumber> {
        let current_timestamp = Utc::now().timestamp() as u64;
        let block_timestamp = consecutive_l1_batches[0].timestamp as u64;
        if block_timestamp + self.deadline_seconds as u64 <= current_timestamp {
            return Some(consecutive_l1_batches[0].number);
        }
        None
    }
}
