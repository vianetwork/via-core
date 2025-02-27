//! This module determines the fees to pay in txs containing blocks submitted to the L1.

use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
};

use tokio::sync::watch;
use via_btc_client::inscriber::Inscriber;
use zksync_config::GasAdjusterConfig;

/// This component keeps track of the median `base_fee` from the last `max_base_fee_samples` blocks
/// and of the median `blob_base_fee` from the last `max_blob_base_fee_sample` blocks.
/// It is used to adjust the base_fee of transactions sent to L1.
#[derive(Debug)]
pub struct ViaGasAdjuster {
    pub(super) base_fee_statistics: GasStatistics<u64>,
    pub(super) config: GasAdjusterConfig,
    pub(super) inscriber: Inscriber,
}

impl ViaGasAdjuster {
    pub async fn new(config: GasAdjusterConfig, inscriber: Inscriber) -> anyhow::Result<Self> {
        // Subtracting 1 from the "latest" block number to prevent errors in case
        // the info about the latest block is not yet present on the node.
        // This sometimes happens on Infura.
        let current_block = inscriber
            .get_client()
            .await
            .fetch_block_height()
            .await?
            .saturating_sub(1) as usize;

        let fee_history = inscriber
            .get_client()
            .await
            .get_fee_history(
                current_block as usize - config.max_base_fee_samples,
                current_block,
            )
            .await?;

        let base_fee_statistics =
            GasStatistics::new(config.max_base_fee_samples, current_block, fee_history);

        Ok(Self {
            base_fee_statistics,
            config,
            inscriber,
        })
    }

    /// Performs an actualization routine for `GasAdjuster`.
    /// This method is intended to be invoked periodically.
    pub async fn keep_updated(&self) -> anyhow::Result<()> {
        let current_block = self
            .inscriber
            .get_client()
            .await
            .fetch_block_height()
            .await?
            .saturating_sub(1) as usize;
        let last_processed_block = self.base_fee_statistics.last_processed_block();

        if current_block > last_processed_block {
            let n_blocks = current_block - last_processed_block;
            let fee_history = self
                .inscriber
                .get_client()
                .await
                .get_fee_history(current_block - n_blocks, current_block)
                .await?;

            self.base_fee_statistics.add_samples(fee_history);
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

    pub async fn run(self: Arc<Self>, stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        loop {
            if *stop_receiver.borrow() {
                tracing::info!("Stop signal received, gas_adjuster is shutting down");
                break;
            }

            if let Err(err) = self.keep_updated().await {
                tracing::warn!("Cannot add the base fee to gas statistics: {}", err);
            }

            tokio::time::sleep(self.config.poll_period()).await;
        }
        Ok(())
    }

    /// Returns the sum of base and priority fee, in wei, not considering time in mempool.
    /// Can be used to get an estimate of current gas price.
    pub(crate) fn estimate_effective_gas_price(&self) -> u64 {
        if let Some(price) = self.config.internal_enforced_l1_gas_price {
            return price;
        }

        let effective_gas_price = self.get_base_fee() + self.get_priority_fee();

        let calculated_price =
            (self.config.internal_l1_pricing_multiplier * effective_gas_price as f64) as u64;

        // Bound the price if it's too high.
        self.bound_gas_price(calculated_price)
    }

    // Todo: investigate the DA layer gas cost
    pub(crate) fn estimate_effective_pubdata_price(&self) -> u64 {
        if let Some(price) = self.config.internal_enforced_pubdata_price {
            return price;
        }
        0
    }

    fn get_base_fee(&self) -> u64 {
        self.base_fee_statistics.median()
    }

    fn get_priority_fee(&self) -> u64 {
        self.config.default_priority_fee_per_gas
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

    pub fn add_samples(&self, fees: impl IntoIterator<Item = T>) {
        self.0.write().unwrap().add_samples(fees)
    }

    pub fn last_processed_block(&self) -> usize {
        self.0.read().unwrap().last_processed_block
    }
}
