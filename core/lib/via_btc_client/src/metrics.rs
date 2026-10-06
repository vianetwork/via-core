use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use vise::{Counter, EncodeLabelSet, Family, Gauge, Metrics};

use crate::types::BitcoinClientResult;

#[derive(Debug, Clone, PartialEq, Eq, Hash, EncodeLabelSet)]
pub struct RpcMethodLabel {
    pub method: String,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_btc_client")]
pub struct ViaBtcClientMetrics {
    /// Number of RPC errors encountered, by method and error type
    pub rpc_errors: Family<RpcMethodLabel, Counter>,

    /// Number of RPC errors encountered, by method and error type
    pub rpc_max_retries_exceeded: Family<RpcMethodLabel, Counter>,
}

#[vise::register]
pub static METRICS: vise::Global<ViaBtcClientMetrics> = vise::Global::new();

#[derive(Debug, Default, Clone, Copy)]
struct InscriptionState {
    unsent: usize,
    inflight: usize,
    overdue: usize,
    unknown: usize,
    first_overdue_l1_batch: i64,
    observed_at: f64,
}

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_inscription")]
struct InscriptionGauges {
    /// Unconfirmed requests without a submission history; not classified by Bitcoin age.
    unsent_requests: Gauge<usize>,
    /// Unconfirmed requests with a submission history, including unknown evidence.
    inflight_requests: Gauge<usize>,
    /// Requests whose latest attempt has Bitcoin block age at least the configured threshold.
    overdue_requests: Gauge<usize>,
    /// Requests with an invalid height or missing batch association; zero overdue is inconclusive.
    unknown_requests: Gauge<usize>,
    /// Lowest known overdue L1 batch number, or zero when none is known. Not a count.
    first_overdue_l1_batch: Gauge<i64>,
    /// Unix seconds at observation start. Require recent evidence before interpreting any gauge.
    observed_at_timestamp_seconds: Gauge<f64>,
}

/// Publishes one per-process inscription snapshot. Preserve scrape target identity across roles.
/// Missing series or an old observation timestamp mean unavailable evidence, not healthy state.
#[derive(Debug, Default)]
pub struct InscriptionObserver(Arc<Mutex<Option<InscriptionState>>>);

impl InscriptionObserver {
    /// Registers the process's single sender; dropping it removes the snapshot from scrapes.
    pub fn register(&self) {
        #[vise::register]
        static COLLECTOR: vise::Collector<Option<InscriptionGauges>> = vise::Collector::new();

        let state = Arc::downgrade(&self.0);
        let result = COLLECTOR.before_scrape(move || {
            let state = (*state.upgrade()?.lock().unwrap())?;
            let gauges = InscriptionGauges::default();
            gauges.unsent_requests.set(state.unsent);
            gauges.inflight_requests.set(state.inflight);
            gauges.overdue_requests.set(state.overdue);
            gauges.unknown_requests.set(state.unknown);
            gauges
                .first_overdue_l1_batch
                .set(state.first_overdue_l1_batch);
            gauges.observed_at_timestamp_seconds.set(state.observed_at);
            Some(gauges)
        });
        if result.is_err() {
            tracing::warn!("Inscription observer registered multiple times");
        }
    }

    /// Invalidates the previous snapshot before processing, including failed or reorg-paused ticks.
    pub fn invalidate(&self) {
        *self.0.lock().unwrap() = None;
    }

    /// Observes one height and one DB snapshot of (batch, latest sent height) per unconfirmed request.
    /// No history means unsent. Height zero is valid; negative or future heights are unknown.
    /// Observation I/O is limited to the budget; errors do not fail transaction processing.
    pub async fn observe(
        &self,
        height: impl Future<Output = BitcoinClientResult<u64>>,
        requests: impl Future<Output = anyhow::Result<Vec<(Option<i64>, Option<i64>)>>>,
        threshold: u32,
        budget: Duration,
    ) {
        self.invalidate();
        let mut state = InscriptionState {
            observed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
            ..InscriptionState::default()
        };
        let observation = async {
            let height = height
                .await
                .context("fetch inscription observation height")?;
            Ok((height, requests.await?))
        };
        match tokio::time::timeout(budget, observation)
            .await
            .context("inscription observation timed out")
            .and_then(|result| result)
        {
            Ok((height, requests)) => {
                for (batch, sent_at) in requests {
                    state.inflight += usize::from(sent_at.is_some());
                    state.unsent += usize::from(sent_at.is_none());
                    let valid_batch = batch.is_some_and(|batch| batch > 0);
                    let age = sent_at
                        .and_then(|sent| u64::try_from(sent).ok())
                        .and_then(|sent| height.checked_sub(sent));
                    if !valid_batch || (sent_at.is_some() && age.is_none()) {
                        state.unknown += 1;
                    }
                    if age.is_some_and(|age| age >= u64::from(threshold)) {
                        state.overdue += 1;
                        if let Some(batch) = batch.filter(|batch| *batch > 0) {
                            if state.first_overdue_l1_batch == 0
                                || batch < state.first_overdue_l1_batch
                            {
                                state.first_overdue_l1_batch = batch;
                            }
                        }
                    }
                }
                *self.0.lock().unwrap() = Some(state);
            }
            Err(err) => tracing::warn!("Failed to observe current inscriptions: {err:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{pending, ready};

    use super::*;

    #[tokio::test]
    async fn inscription_snapshot_lifecycle() {
        let observer = InscriptionObserver::default();
        observer.register();
        let registry = vise::MetricsCollection::default()
            .filter(|group| group.name == "InscriptionGauges")
            .collect();
        let scrape = || {
            let mut output = String::new();
            registry
                .encode(&mut output, vise::Format::OpenMetrics)
                .unwrap();
            output
        };
        assert_eq!(scrape(), "# EOF\n");
        assert!(observer.0.lock().unwrap().is_none());
        let budget = Duration::from_secs(1);
        for (height, overdue, first) in [(111, 0, 0), (112, 1, 9), (113, 2, 4)] {
            observer
                .observe(
                    ready(Ok(height)),
                    ready(Ok(vec![
                        (Some(9), Some(100)),
                        (Some(4), Some(101)),
                        (Some(2), None),
                    ])),
                    12,
                    budget,
                )
                .await;
            let state = observer.0.lock().unwrap().unwrap();
            assert_eq!((state.inflight, state.unsent, state.unknown), (2, 1, 0));
            assert_eq!(
                (state.overdue, state.first_overdue_l1_batch),
                (overdue, first)
            );
            assert!(scrape()
                .lines()
                .any(|line| line == format!("via_inscription_overdue_requests {overdue}")));
            assert!(scrape()
                .lines()
                .any(|line| line == format!("via_inscription_first_overdue_l1_batch {first}")));
        }
        observer
            .observe(
                ready(Ok(12)),
                ready(Ok(vec![
                    (Some(9), Some(0)),
                    (None, Some(0)),
                    (Some(2), Some(-1)),
                    (Some(3), Some(13)),
                    (None, None),
                    (Some(11), Some(1)),
                ])),
                12,
                budget,
            )
            .await;
        let state = observer.0.lock().unwrap().unwrap();
        assert_eq!(
            (state.inflight, state.unsent, state.overdue, state.unknown),
            (5, 1, 2, 4)
        );
        assert_eq!(state.first_overdue_l1_batch, 9);
        assert!(scrape().contains("via_inscription_inflight_requests 5\n"));
        assert!(scrape().contains("via_inscription_unsent_requests 1\n"));
        assert!(scrape().contains("via_inscription_unknown_requests 4\n"));

        observer
            .observe(ready(Ok(113)), ready(Ok(vec![])), 12, budget)
            .await;
        let recovered = observer.0.lock().unwrap().unwrap();
        assert_eq!(
            (
                recovered.inflight,
                recovered.overdue,
                recovered.first_overdue_l1_batch
            ),
            (0, 0, 0)
        );
        assert!(recovered.observed_at > 0.0);
        let recovered_scrape = scrape();
        assert!(recovered_scrape.contains("via_inscription_overdue_requests 0\n"));
        tokio::task::yield_now().await;
        assert_eq!(scrape(), recovered_scrape);
        assert_eq!(
            observer.0.lock().unwrap().unwrap().observed_at,
            recovered.observed_at
        );
        observer.invalidate();
        assert!(observer.0.lock().unwrap().is_none());
        assert_eq!(scrape(), "# EOF\n");

        for failure in 0..3 {
            observer
                .observe(ready(Ok(113)), ready(Ok(vec![])), 12, budget)
                .await;
            observer
                .observe(
                    async {
                        if failure == 0 {
                            Err(crate::types::BitcoinError::Other("RPC unavailable".into()))
                        } else {
                            Ok(113)
                        }
                    },
                    async {
                        if failure == 1 {
                            anyhow::bail!("DB unavailable");
                        }
                        pending().await
                    },
                    12,
                    Duration::from_millis(1),
                )
                .await;
            assert!(observer.0.lock().unwrap().is_none());
            assert_eq!(scrape(), "# EOF\n");
        }
        observer
            .observe(ready(Ok(0)), ready(Ok(vec![(Some(9), Some(0))])), 0, budget)
            .await;
        assert!(scrape().contains("via_inscription_overdue_requests 1\n"));
        drop(observer);
        assert_eq!(scrape(), "# EOF\n");
    }
}
