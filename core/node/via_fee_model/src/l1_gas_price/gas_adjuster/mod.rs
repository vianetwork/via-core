use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
};

use tokio::sync::watch;
use via_btc_client::traits::BitcoinOps;
use zksync_config::configs::via_btc_sender::ViaGasAdjusterConfig;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct ViaGasAdjuster {
    config: ViaGasAdjusterConfig,
    client: Box<dyn BitcoinOps>,
    pub(super) base_fee_statistics: GasStatistics<u64>,
}

impl ViaGasAdjuster {
    pub fn new(client: Box<dyn BitcoinOps>, config: ViaGasAdjusterConfig) -> Self {
        let base_fee_statistics: GasStatistics<u64> =
            GasStatistics::new(config.max_base_fee_samples);
        Self {
            config,
            client,
            base_fee_statistics,
        }
    }

    /// Performs an actualization routine for `GasAdjuster`.
    /// This method is intended to be invoked periodically.
    pub async fn keep_updated(&self) -> anyhow::Result<()> {
        let current_block = self.client.fetch_block_height().await?.saturating_sub(1) as usize;

        let last_processed_block = self.base_fee_statistics.last_processed_block() + 1;

        if current_block > last_processed_block {
            let fee_history = self
                .client
                .get_fee_history(last_processed_block, current_block)
                .await?;

            self.base_fee_statistics
                .add_samples(fee_history.iter().map(|fee| *fee));
        }

        Ok(())
    }

    pub async fn run(self: Arc<Self>, stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let block_height = self.client.fetch_block_height().await? as usize;

        let fee_history = self
            .client
            .get_fee_history(
                block_height - self.config.max_base_fee_samples,
                block_height,
            )
            .await?;

        self.base_fee_statistics.add_samples(fee_history);
        self.base_fee_statistics
            .set_last_processed_block(block_height);

        loop {
            if *stop_receiver.borrow() {
                tracing::info!("Stop signal received, via_gas_adjuster is shutting down");
                break;
            }

            if let Err(err) = self.keep_updated().await {
                tracing::warn!("Cannot add the base fee to gas statistics: {}", err);
            }

            tokio::time::sleep(self.config.poll_period).await;
        }
        Ok(())
    }

    fn bound_gas_price(&self, gas_price: u64) -> u64 {
        let max_l1_gas_price = self.config.max_l1_gas_price();
        if gas_price > max_l1_gas_price {
            tracing::warn!(
                "Effective gas price is too high: {gas_price}, using max allowed: {}",
                max_l1_gas_price
            );
            return max_l1_gas_price;
        }
        gas_price
    }

    /// Returns the sum of base and priority fee, in wei, not considering time in mempool.
    /// Can be used to get an estimate of current gas price.
    pub(crate) fn estimate_effective_gas_price(&self) -> u64 {
        if let Some(price) = self.config.internal_enforced_l1_gas_price {
            return price;
        }

        let effective_gas_price = self.base_fee_statistics.median();

        let calculated_price =
            (self.config.internal_l1_pricing_multiplier * effective_gas_price as f64) as u64;

        // Bound the price if it's too high.
        self.bound_gas_price(calculated_price)
    }

    /// Fix this when we have a better understanding of dynamic pricing for custom DA layers.
    /// GitHub issue: https://github.com/matter-labs/zksync-era/issues/2105
    pub(crate) fn estimate_effective_pubdata_price(&self) -> u64 {
        if let Some(price) = self.config.internal_enforced_pubdata_price {
            return price;
        }
        0
    }

    #[cfg(test)]
    pub fn set_client(&mut self, client: Box<dyn BitcoinOps>) {
        self.client = client;
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
    fn new(max_samples: usize) -> Self {
        let statistics: GasStatisticsInner<T> = Self {
            max_samples,
            samples: VecDeque::with_capacity(max_samples),
            median_cached: T::default(),
            last_processed_block: 0,
        };

        Self { ..statistics }
    }

    fn set_last_processed_block(&mut self, block: usize) {
        self.last_processed_block = block;
    }

    fn median(&self) -> T {
        self.median_cached
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
    pub fn new(max_samples: usize) -> Self {
        Self(RwLock::new(GasStatisticsInner::new(max_samples)))
    }

    pub fn median(&self) -> T {
        self.0.read().unwrap().median()
    }

    pub fn add_samples(&self, fees: impl IntoIterator<Item = T>) {
        self.0.write().unwrap().add_samples(fees)
    }

    pub fn set_last_processed_block(&self, block: usize) {
        self.0.write().unwrap().set_last_processed_block(block);
    }

    pub fn last_processed_block(&self) -> usize {
        self.0.read().unwrap().last_processed_block
    }
}
