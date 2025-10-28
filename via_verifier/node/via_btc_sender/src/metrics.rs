use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::NaiveDateTime;
use via_verifier_dal::{Connection, Verifier, VerifierDal};
use vise::{
    Buckets, Counter, EncodeLabelSet, EncodeLabelValue, Family, Gauge, Histogram, Metrics, Unit,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelSet, EncodeLabelValue)]
#[metrics(label = "block_number_variant", rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub(super) enum L1BatchVariant {
    Indexed,
    Voted,
    Finalized,
    Rejected,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_verifier_btc_sender")]
pub struct ViaBtcSenderMetrics {
    /// Latency of collecting Ethereum sender metrics.
    #[metrics(buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    metrics_latency: Histogram<Duration>,

    /// Time taken to broadcast a transaction
    #[metrics(buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    pub broadcast_time: Histogram<Duration>,

    /// Time between inscription submission and confirmation
    #[metrics(buckets = Buckets::exponential(60.0..=86400.0, 5.0), unit = vise::Unit::Seconds)]
    pub inscription_confirmation_time: Histogram<Duration>,

    /// Number of inflight inscriptions (submitted but not confirmed)
    pub inflight_inscriptions: Gauge<usize>,

    /// The first l1_batch number blocked inscription
    pub report_blocked_l1_batch_inscription: Gauge<usize>,

    /// Error when broadcast a transaction.
    pub l1_transient_errors: Counter,

    /// Error when broadcast a transaction.
    pub last_known_l1_block: Family<L1BatchVariant, Gauge<usize>>,

    /// The BTC balance of the account used to created inscriptions.
    pub btc_sender_account_balance: Gauge<usize>,

    /// Errors
    pub errors: Counter,
}

impl ViaBtcSenderMetrics {
    pub async fn track_block_numbers(
        &self,
        connection: &mut Connection<'_, Verifier>,
    ) -> anyhow::Result<()> {
        let metrics_latency = self.metrics_latency.start();

        if let Some(last_finalized_l1_batch_numnber) = connection
            .via_votes_dal()
            .get_last_finalized_l1_batch()
            .await?
        {
            self.last_known_l1_block[&L1BatchVariant::Finalized]
                .set(last_finalized_l1_batch_numnber as usize);
        }

        let last_voted_l1_batch_numnber =
            connection.via_votes_dal().get_last_voted_l1_batch().await?;

        self.last_known_l1_block[&L1BatchVariant::Voted].set(last_voted_l1_batch_numnber as usize);

        if let Some((rejected_l1_batch_numnber, _)) = connection
            .via_votes_dal()
            .get_first_rejected_l1_batch()
            .await?
        {
            self.last_known_l1_block[&L1BatchVariant::Rejected]
                .set(rejected_l1_batch_numnber as usize);
        }

        let last_indexed_l1_batch_numnber = connection
            .via_votes_dal()
            .get_last_votable_l1_batch()
            .await?;

        self.last_known_l1_block[&L1BatchVariant::Indexed]
            .set(last_indexed_l1_batch_numnber as usize);

        metrics_latency.observe();
        Ok(())
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
