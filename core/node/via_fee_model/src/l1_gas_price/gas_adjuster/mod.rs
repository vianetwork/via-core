use std::{collections::VecDeque, sync::RwLock};

use via_btc_client::traits::BitcoinOps;
use zksync_da_client::DataAvailabilityClient;

#[derive(Debug)]
pub struct GasAdjuster {
    da_client: Box<dyn DataAvailabilityClient>,
    btc_client: Box<dyn BitcoinOps>,
}

impl GasAdjuster {
    pub fn new(
        da_client: Box<dyn DataAvailabilityClient>,
        btc_client: Box<dyn BitcoinOps>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            da_client,
            btc_client,
        })
    }
}

/// Helper structure responsible for collecting the data about recent transactions,
/// calculating the median base fee.
#[derive(Debug, Clone, Default)]
pub(super) struct GasStatisticsInner<T> {
    samples: VecDeque<T>,
    median_cached: T,
    max_samples: usize,
    last_processed_block: usize,
}

impl<T: Ord + Copy + Default> GasStatisticsInner<T> {
    fn new(max_samples: usize, block: usize, fee_history: impl IntoIterator<Item = T>) -> Self {
        let mut statistics = Self {
            max_samples,
            samples: VecDeque::with_capacity(max_samples),
            median_cached: T::default(),
            last_processed_block: 0,
        };

        statistics.add_samples(fee_history);

        Self {
            last_processed_block: block,
            ..statistics
        }
    }

    fn median(&self) -> T {
        self.median_cached
    }

    fn last_added_value(&self) -> T {
        self.samples.back().copied().unwrap_or(self.median_cached)
    }

    fn add_samples(&mut self, fees: impl IntoIterator<Item = T>) {
        let old_len = self.samples.len();
        self.samples.extend(fees);
        let processed_blocks = self.samples.len() - old_len;
        self.last_processed_block += processed_blocks;

        let extra = self.samples.len().saturating_sub(self.max_samples);
        self.samples.drain(..extra);

        let mut samples: Vec<_> = self.samples.iter().cloned().collect();

        if !self.samples.is_empty() {
            let (_, &mut median, _) = samples.select_nth_unstable(self.samples.len() / 2);
            self.median_cached = median;
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct GasStatistics<T>(RwLock<GasStatisticsInner<T>>);

impl<T: Ord + Copy + Default> GasStatistics<T> {
    pub fn new(max_samples: usize, block: usize, fee_history: impl IntoIterator<Item = T>) -> Self {
        Self(RwLock::new(GasStatisticsInner::new(
            max_samples,
            block,
            fee_history,
        )))
    }

    pub fn median(&self) -> T {
        self.0.read().unwrap().median()
    }

    pub fn last_added_value(&self) -> T {
        self.0.read().unwrap().last_added_value()
    }

    pub fn add_samples(&self, fees: impl IntoIterator<Item = T>) {
        self.0.write().unwrap().add_samples(fees)
    }

    pub fn last_processed_block(&self) -> usize {
        self.0.read().unwrap().last_processed_block
    }
}
