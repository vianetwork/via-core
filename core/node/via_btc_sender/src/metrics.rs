use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::NaiveDateTime;
use vise::{
    Buckets, Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, Histogram, LabeledFamily,
    Metrics, Unit,
};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_shared_metrics::{BlockL1Stage, BlockStage, APP_METRICS};
use zksync_types::{
    aggregated_operations::AggregatedActionType,
    btc_inscription_operations::ViaBtcInscriptionRequestType, L1BatchNumber,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelSet, EncodeLabelValue)]
#[metrics(label = "block_number_variant", rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub(super) enum BlockNumberVariant {
    Latest,
    Finalized,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_btc_sender")]
pub struct ViaBtcSenderMetrics {
    /// Latency of collecting Ethereum sender metrics.
    #[metrics(buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    metrics_latency: Histogram<Duration>,

    /// Time taken to broadcast a transaction
    #[metrics(buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    pub broadcast_time: Histogram<Duration>,

    /// Time taken to prepare inscription data
    #[metrics(buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    pub inscription_preparation_time: Histogram<Duration>,

    /// Time between inscription submission and confirmation
    #[metrics(buckets = Buckets::exponential(60.0..=86400.0, 5.0), unit = vise::Unit::Seconds)]
    pub inscription_confirmation_time: Histogram<Duration>,

    /// Number of pending inscription requests (not yet submitted)
    pub pending_inscription_requests: Gauge<usize>,

    /// Number of inflight inscriptions (submitted but not confirmed)
    pub inflight_inscriptions: Gauge<usize>,

    /// The first l1_batch number blocked inscription.
    pub report_blocked_l1_batch_inscription: Gauge<usize>,

    /// Error when broadcast a transaction.
    pub l1_transient_errors: Counter,

    /// Aggregator errors.
    pub aggregator_errors: Counter,

    /// Manager errors.
    pub manager_errors: Counter,

    /// Last L1 block observed by the Ethereum sender.
    pub last_known_l1_block: Family<BlockNumberVariant, Gauge<usize>>,

    /// The BTC balance of the account used to created inscriptions.
    #[metrics(labels = ["address"])]
    pub btc_sender_account_balance: LabeledFamily<String, Gauge<usize>>,
}

impl ViaBtcSenderMetrics {
    pub async fn track_block_numbers(&self, connection: &mut Connection<'_, Core>) {
        let metrics_latency = self.metrics_latency.start();

        let finalized_l1_batch_numnber = connection
            .via_blocks_dal()
            .get_last_finalized_l1_batch()
            .await
            .unwrap_or(0);
        let last_l1_batch_numnber = connection
            .blocks_dal()
            .get_sealed_l1_batch_number()
            .await
            .unwrap_or(Some(L1BatchNumber::from(0)))
            .unwrap()
            .0;
        self.last_known_l1_block[&BlockNumberVariant::Latest].set(last_l1_batch_numnber as usize);
        self.last_known_l1_block[&BlockNumberVariant::Finalized]
            .set(finalized_l1_batch_numnber as usize);

        metrics_latency.observe();
    }

    pub async fn track_btc_tx_metrics(
        &self,
        connection: &mut Connection<'_, Core>,
        l1_stage: BlockL1Stage,
        inscriptions: Vec<(u32, ViaBtcInscriptionRequestType)>,
    ) {
        let metrics_latency = self.metrics_latency.start();
        for inscription in inscriptions {
            let stage = BlockStage::L1 {
                l1_stage,
                tx_type: AggregatedActionType::from(inscription.1),
            };

            let l1_batches_statistics = connection
                .via_blocks_dal()
                .get_l1_batches_statistics_for_inscription_tx_id(inscription.0)
                .await
                .unwrap();

            // This should be only the case when some blocks were reverted.
            if l1_batches_statistics.is_empty() {
                tracing::warn!(
                    "No L1 batches were found for btc_tx with id = {}",
                    inscription.0
                );
                return;
            }

            for statistics in l1_batches_statistics {
                APP_METRICS.block_latency[&stage].observe(Duration::from_secs(
                    seconds_since_epoch() - statistics.timestamp,
                ));
                APP_METRICS.processed_txs[&stage.into()]
                    .inc_by(statistics.l2_tx_count as u64 + statistics.l1_tx_count as u64);
                APP_METRICS.processed_l1_txs[&stage.into()].inc_by(statistics.l1_tx_count as u64);
            }
        }
        metrics_latency.observe();
    }

    pub fn track_inscription_confirmation(&self, created_at: NaiveDateTime) {
        let confirmation_delay = seconds_since_epoch() - created_at.and_utc().timestamp() as u64;

        self.inscription_confirmation_time
            .observe(Duration::from_secs(confirmation_delay));
    }
}

#[vise::register]
pub static METRICS: vise::Global<ViaBtcSenderMetrics> = vise::Global::new();

fn seconds_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Incorrect system time")
        .as_secs()
}
