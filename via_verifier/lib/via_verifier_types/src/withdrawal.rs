use bitcoin::{Address as BitcoinAddress, Amount};
use indexmap::IndexMap;
use via_btc_client::indexer::withdrawal::L1Withdrawal;
use zksync_types::ethabi::Address;

#[derive(Debug, Clone)]
pub struct WithdrawalRequest {
    pub id: String,
    pub receiver: BitcoinAddress,
    pub amount: Amount,
    pub l2_sender: Address,
    pub l2_tx_hash: String,
    pub l2_tx_log_index: u16,
}

impl WithdrawalRequest {
    pub fn group_withdrawals_by_address(
        withdrawals: Vec<WithdrawalRequest>,
    ) -> anyhow::Result<IndexMap<BitcoinAddress, Amount>> {
        // Group withdrawals by address and sum amounts
        let mut grouped_withdrawals: IndexMap<BitcoinAddress, Amount> = IndexMap::new();

        for w in withdrawals {
            let entry = grouped_withdrawals
                .entry(w.receiver)
                .or_insert(Amount::ZERO);
            *entry = entry
                .checked_add(w.amount)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
        }
        Ok(grouped_withdrawals)
    }
}

impl From<L1Withdrawal> for WithdrawalRequest {
    fn from(l1: L1Withdrawal) -> Self {
        WithdrawalRequest {
            id: l1.l2_meta.l2_id,
            receiver: l1.receiver,
            amount: l1.value,
            l2_sender: Address::zero(),
            l2_tx_hash: "".into(),
            l2_tx_log_index: l1.l2_meta.l2_tx_event_index,
        }
    }
}

pub fn get_withdrawal_requests(l1_withdrawals: Vec<L1Withdrawal>) -> Vec<WithdrawalRequest> {
    let mut withdrawals = Vec::with_capacity(l1_withdrawals.len());

    for w in l1_withdrawals {
        withdrawals.push(WithdrawalRequest::from(w));
    }
    withdrawals
}
