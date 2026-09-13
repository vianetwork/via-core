use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use vise::{Counter, EncodeLabelSet, Family, Gauge, LabeledFamily, Metrics};
use zksync_types::via_btc_sender::ViaBtcInscriptionStatus;

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

type InscriptionSnapshot = Option<(ViaBtcInscriptionStatus, f64)>;

static OBSERVERS: Mutex<BTreeMap<&'static str, Weak<Mutex<InscriptionSnapshot>>>> =
    Mutex::new(BTreeMap::new());

#[vise::register]
static INSCRIPTIONS: vise::Collector<InscriptionMetrics> = vise::Collector::new();

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_btc_sender_inscription")]
struct InscriptionMetrics {
    /// Unconfirmed inscription requests.
    #[metrics(labels = ["sender"])]
    pending: LabeledFamily<&'static str, Gauge>,
    /// Requests whose latest sent attempt is at least the configured Bitcoin block age.
    #[metrics(labels = ["sender"])]
    overdue: LabeledFamily<&'static str, Gauge>,
    /// Requests with unavailable age or L1 batch association; these can also be overdue.
    #[metrics(labels = ["sender"])]
    unobserved: LabeledFamily<&'static str, Gauge>,
    /// Smallest associated overdue L1 batch number, or zero.
    #[metrics(labels = ["sender"])]
    first_overdue_batch: LabeledFamily<&'static str, Gauge>,
    /// UNIX time when the successful observation started. Missing or stale values are unknown.
    #[metrics(labels = ["sender"])]
    observed_at_timestamp_seconds: LabeledFamily<&'static str, Gauge<f64>>,
}

impl InscriptionMetrics {
    fn collect() -> Self {
        let metrics = Self::default();
        OBSERVERS.lock().unwrap().retain(|sender, observer| {
            let Some(observer) = observer.upgrade() else {
                return false;
            };
            let snapshot = *observer.lock().unwrap();
            if let Some((status, observed_at)) = snapshot {
                metrics.pending[sender].set(status.pending);
                metrics.overdue[sender].set(status.overdue);
                metrics.unobserved[sender].set(status.unobserved);
                metrics.first_overdue_batch[sender].set(status.first_overdue_batch);
                metrics.observed_at_timestamp_seconds[sender].set(observed_at);
            }
            true
        });
        metrics
    }
}

/// Treat missing or stale observations as unknown. Successful empty observations expose zeros.
#[derive(Debug)]
pub struct InscriptionObserver {
    sender: &'static str,
    snapshot: Arc<Mutex<InscriptionSnapshot>>,
}

impl InscriptionObserver {
    /// Registers the `main` or `verifier` sender, replacing any previous observer for that role.
    pub fn new(sender: &'static str) -> Self {
        let snapshot = Arc::new(Mutex::new(None));
        OBSERVERS
            .lock()
            .unwrap()
            .insert(sender, Arc::downgrade(&snapshot));
        let _ = INSCRIPTIONS.before_scrape(InscriptionMetrics::collect);
        Self { sender, snapshot }
    }

    /// Publishes a complete observation or removes it on failure. Blocking work can exceed the timeout.
    pub async fn refresh(
        &self,
        observe: impl Future<Output = anyhow::Result<ViaBtcInscriptionStatus>>,
        timeout: Duration,
    ) {
        let started = Instant::now();
        let result: anyhow::Result<_> = async {
            let observed_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("inscription observation clock precedes UNIX epoch")?
                .as_secs_f64();
            let status = tokio::time::timeout(timeout, observe)
                .await
                .context("inscription observation timed out")??;
            anyhow::ensure!(
                started.elapsed() <= timeout,
                "inscription observation exceeded {timeout:?}"
            );
            Ok((status, observed_at))
        }
        .await;
        let snapshot = match result {
            Ok(snapshot) => Some(snapshot),
            Err(err) => {
                tracing::warn!(sender = self.sender, "Cannot observe inscriptions: {err:#}");
                None
            }
        };
        *self.snapshot.lock().unwrap() = snapshot;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrape(sender: &str) -> BTreeMap<String, f64> {
        let mut registry = vise::Registry::empty();
        registry.register_collector(&INSCRIPTIONS);
        let mut output = String::new();
        registry
            .encode(&mut output, vise::Format::OpenMetrics)
            .unwrap();
        let label = format!("{{sender=\"{sender}\"}} ");
        output
            .lines()
            .filter_map(|line| {
                let line = line.strip_prefix("via_btc_sender_inscription_")?;
                let (name, value) = line.split_once(&label)?;
                Some((name.to_owned(), value.parse().unwrap()))
            })
            .collect()
    }

    #[tokio::test]
    async fn failure_and_recovery_publish_complete_snapshots() {
        let observer = InscriptionObserver::new("recovery_test");
        assert!(scrape(observer.sender).is_empty());
        let status = ViaBtcInscriptionStatus {
            pending: 3,
            overdue: 2,
            unobserved: 1,
            first_overdue_batch: 7,
        };
        observer
            .refresh(async { Ok(status) }, Duration::from_secs(1))
            .await;
        let snapshot = scrape(observer.sender);
        assert_eq!(snapshot.len(), 5);
        for (name, value) in [
            ("pending", 3.0),
            ("overdue", 2.0),
            ("unobserved", 1.0),
            ("first_overdue_batch", 7.0),
        ] {
            assert_eq!(snapshot[name], value);
        }
        assert!(snapshot["observed_at_timestamp_seconds"] > 0.0);
        observer
            .refresh(
                async { anyhow::bail!("database unavailable") },
                Duration::from_secs(1),
            )
            .await;
        assert!(scrape(observer.sender).is_empty());
        observer
            .refresh(
                async { Ok(ViaBtcInscriptionStatus::default()) },
                Duration::from_secs(1),
            )
            .await;
        let snapshot = scrape(observer.sender);
        assert_eq!(snapshot.len(), 5);
        for name in ["pending", "overdue", "unobserved", "first_overdue_batch"] {
            assert_eq!(snapshot[name], 0.0);
        }
    }

    #[tokio::test]
    async fn timeouts_invalidate_even_non_yielding_observations() {
        let observer = InscriptionObserver::new("timeout_test");
        observer
            .refresh(
                async { Ok(ViaBtcInscriptionStatus::default()) },
                Duration::from_secs(1),
            )
            .await;
        assert_eq!(scrape(observer.sender).len(), 5);
        observer
            .refresh(std::future::pending(), Duration::from_millis(1))
            .await;
        assert!(scrape(observer.sender).is_empty());
        observer
            .refresh(
                async {
                    std::thread::sleep(Duration::from_millis(5));
                    Ok(ViaBtcInscriptionStatus::default())
                },
                Duration::from_millis(1),
            )
            .await;
        assert!(scrape(observer.sender).is_empty());
    }

    #[tokio::test]
    async fn refresh_uses_start_time_and_preserves_it_between_scrapes() {
        let observer = InscriptionObserver::new("timestamp_test");
        let mut finished = 0.0;
        observer
            .refresh(
                async {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    finished = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs_f64();
                    Ok(ViaBtcInscriptionStatus::default())
                },
                Duration::from_secs(1),
            )
            .await;
        let snapshot = scrape(observer.sender);
        assert!(snapshot["observed_at_timestamp_seconds"] <= finished - 0.01);
        tokio::time::sleep(Duration::from_millis(2)).await;
        assert_eq!(scrape(observer.sender), snapshot);
    }

    #[test]
    fn concurrent_publication_never_mixes_snapshot_fields() {
        let observer = InscriptionObserver::new("coherence_test");
        let state = Arc::clone(&observer.snapshot);
        let writer = std::thread::spawn(move || {
            for value in 1..=5000 {
                *state.lock().unwrap() = Some((
                    ViaBtcInscriptionStatus {
                        pending: value,
                        overdue: value,
                        unobserved: value,
                        first_overdue_batch: value,
                    },
                    value as f64,
                ));
                std::thread::yield_now();
            }
        });
        for _ in 0..1000 {
            let snapshot = scrape(observer.sender);
            if let Some(value) = snapshot.values().next() {
                assert_eq!(snapshot.len(), 5);
                assert!(snapshot.values().all(|other| other == value));
            }
        }
        writer.join().unwrap();
        assert_eq!(scrape(observer.sender).len(), 5);
    }

    #[tokio::test]
    async fn roles_and_replacement_observers_remain_independent() {
        let main = InscriptionObserver::new("main");
        let verifier = InscriptionObserver::new("verifier");
        for (observer, pending) in [(&main, 1), (&verifier, 2)] {
            observer
                .refresh(
                    async {
                        Ok(ViaBtcInscriptionStatus {
                            pending,
                            ..Default::default()
                        })
                    },
                    Duration::from_secs(1),
                )
                .await;
        }
        assert_eq!(scrape("main")["pending"], 1.0);
        assert_eq!(scrape("verifier")["pending"], 2.0);
        let replacement = InscriptionObserver::new("main");
        assert!(scrape("main").is_empty());
        assert_eq!(scrape("verifier").len(), 5);
        main.refresh(
            async { Ok(ViaBtcInscriptionStatus::default()) },
            Duration::from_secs(1),
        )
        .await;
        assert!(scrape("main").is_empty());
        replacement
            .refresh(
                async { Ok(ViaBtcInscriptionStatus::default()) },
                Duration::from_secs(1),
            )
            .await;
        drop(main);
        assert_eq!(scrape("main").len(), 5);
        drop(replacement);
        assert!(scrape("main").is_empty());
    }
}
