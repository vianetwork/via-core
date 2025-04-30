use crate::types::BitcoinNetwork;

/// Provides network-specific fee rate limits for Bitcoin transactions
///
/// These limits help protect against fee estimation spikes and drops, particularly on testnet
/// where fee estimates can be unreliable due to inconsistent mining patterns.
#[derive(Debug, Clone, Copy)]
pub struct FeeRateLimits {
    min_fee_rate: u64,
    max_fee_rate: u64,
}

impl FeeRateLimits {
    /// Create fee rate limits (in sat/vB) appropriate for the given Bitcoin network
    pub fn from_network(network: BitcoinNetwork) -> Self {
        match network {
            // Limits based on the data from https://dune.com/dataalways/bitcoin-fee-tracker
            BitcoinNetwork::Bitcoin => Self {
                min_fee_rate: 1,
                max_fee_rate: 100,
            },
            BitcoinNetwork::Testnet => Self {
                min_fee_rate: 1,
                max_fee_rate: 100,
            },
            _ => Self {
                min_fee_rate: 1,
                max_fee_rate: 10,
            },
        }
    }

    /// Get the minimum fee rate for the network (in sat/vB)
    pub fn min_fee_rate(&self) -> u64 {
        self.min_fee_rate
    }

    /// Get the maximum fee rate for the network (in sat/vB)
    pub fn max_fee_rate(&self) -> u64 {
        self.max_fee_rate
    }
}
