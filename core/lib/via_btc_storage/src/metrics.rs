use std::time::Duration;

use vise::{Buckets, Counter, Gauge, Histogram, LabeledFamily, Metrics, Unit};

#[derive(Debug, Metrics)]
#[metrics(prefix = "via_bitcoin_reader")]
pub struct ReaderMetrics {
    #[metrics(labels = ["role"])]
    pub head: LabeledFamily<&'static str, Gauge<usize>, 1>,
    #[metrics(labels = ["role"])]
    pub frontier: LabeledFamily<&'static str, Gauge<usize>, 1>,
    #[metrics(labels = ["role"])]
    pub replay_ready: LabeledFamily<&'static str, Gauge<usize>, 1>,
    #[metrics(labels = ["role"])]
    pub replay_unreconciled: LabeledFamily<&'static str, Gauge<usize>, 1>,
    #[metrics(labels = ["role"])]
    pub stopped: LabeledFamily<&'static str, Gauge<usize>, 1>,
    #[metrics(labels = ["role", "consumer"])]
    pub consumer: LabeledFamily<(&'static str, &'static str), Gauge<usize>, 2>,
    #[metrics(labels = ["role"], buckets = Buckets::LATENCIES, unit = Unit::Seconds)]
    pub inscription_time: LabeledFamily<&'static str, Histogram<Duration>, 1>,
    #[metrics(labels = ["role"])]
    pub inscription_errors: LabeledFamily<&'static str, Counter, 1>,
    #[metrics(labels = ["role"])]
    pub pending: LabeledFamily<&'static str, Gauge<usize>, 1>,
    #[metrics(labels = ["role"])]
    pub in_flight: LabeledFamily<&'static str, Gauge<usize>, 1>,
    #[metrics(labels = ["role", "address"])]
    pub balance: LabeledFamily<(&'static str, String), Gauge<usize>, 2>,
    #[metrics(labels = ["role"], buckets = Buckets::exponential(60.0..=86400.0, 5.0), unit = Unit::Seconds)]
    pub confirmation_time: LabeledFamily<&'static str, Histogram<Duration>, 1>,
}
#[vise::register]
pub static METRICS: vise::Global<ReaderMetrics> = vise::Global::new();

impl ReaderMetrics {
    pub fn confirmed(&self, role: &'static str, created_at: chrono::NaiveDateTime) {
        let delay = chrono::Utc::now().timestamp().saturating_sub(created_at.and_utc().timestamp());
        self.confirmation_time[&role].observe(Duration::from_secs(delay.max(0) as u64));
    }
}
