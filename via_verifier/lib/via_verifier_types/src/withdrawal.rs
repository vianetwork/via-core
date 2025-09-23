use bitcoin::{Address as BitcoinAddress, Amount};
use indexmap::IndexMap;
use zksync_types::Address;

#[derive(Debug, Clone)]
pub struct WithdrawalRequest {
    pub address: BitcoinAddress,
    pub amount: Amount,
    pub l2_sender: Address,
    pub l2_tx_hash: String,
    pub l2_tx_log_index: i64,
}

impl WithdrawalRequest {
    pub fn group_withdrawals_by_address(
        withdrawals: Vec<WithdrawalRequest>,
    ) -> anyhow::Result<IndexMap<BitcoinAddress, Amount>> {
        // Group withdrawals by address and sum amounts
        let mut grouped_withdrawals: IndexMap<BitcoinAddress, Amount> = IndexMap::new();

        for w in withdrawals {
            let entry = grouped_withdrawals.entry(w.address).or_insert(Amount::ZERO);
            *entry = entry
                .checked_add(w.amount)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
        }
        Ok(grouped_withdrawals)
    }
}
