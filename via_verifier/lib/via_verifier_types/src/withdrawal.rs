use bitcoin::{Address, Amount};
use indexmap::IndexMap;

#[derive(Debug, Clone)]
pub struct WithdrawalRequest {
    pub address: Address,
    pub amount: Amount,
}

impl WithdrawalRequest {
    pub fn group_withdrawals_by_address(
        withdrawals: Vec<WithdrawalRequest>,
    ) -> anyhow::Result<IndexMap<Address, Amount>> {
        // Group withdrawals by address and sum amounts
        let mut grouped_withdrawals: IndexMap<Address, Amount> = IndexMap::new();

        for w in withdrawals {
            let entry = grouped_withdrawals.entry(w.address).or_insert(Amount::ZERO);
            *entry = entry
                .checked_add(w.amount)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
        }
        Ok(grouped_withdrawals)
    }
}
