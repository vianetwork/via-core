use bitcoin::{Address as BitcoinAddress, Amount};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use zksync_types::{api::TransactionReceipt, ethabi::Address, H256};

/// Expected gross obligation; an observed payment never replaces this amount.
/// BTCPay Server's separate invoice/payment amounts are the design precedent,
/// not its invoice accounting or payment-status policy:
/// https://github.com/btcpayserver/btcpayserver/blob/a305e951761784e65834f03f082cb880b93b99b0/BTCPayServer.Data/Data/InvoiceData.cs
/// https://github.com/btcpayserver/btcpayserver/blob/a305e951761784e65834f03f082cb880b93b99b0/BTCPayServer.Data/Data/PaymentData.cs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WithdrawalRequest {
    pub id: String,
    #[serde(deserialize_with = "deserialize_address")]
    pub receiver: BitcoinAddress,
    pub amount: Amount,
    pub l2_sender: Address,
    pub l2_tx_hash: H256,
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

/// A fully associated withdrawal whose receiver cannot be paid on the configured network.
/// This is retained evidence, never an eligible payment request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NonPayableWithdrawal {
    pub id: String,
    pub l2_tx_hash: H256,
    pub l2_tx_log_index: u16,
    pub l2_sender: Address,
    /// Gross satoshis after the contract's full-width floor division.
    pub amount: Amount,
    pub raw_receiver: Vec<u8>,
    pub reason: NonPayableWithdrawalReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NonPayableWithdrawalReason {
    InvalidUtf8,
    InvalidAddress,
    WrongNetwork,
}

/// Complete evidence from the configured trusted L2 source and the retained DA blob.
/// Completeness is established by the importer, not by a zero withdrawal count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompleteWithdrawalBatch {
    pub batch_number: u32,
    pub chain_id: u64,
    pub network: bitcoin::Network,
    pub protocol_version: u16,
    pub blob_id: String,
    pub pubdata_hash: H256,
    pub start_block: u64,
    pub end_block: u64,
    pub pubdata: Vec<u8>,
    pub receipts: Vec<TransactionReceipt>,
    pub withdrawals: Vec<WithdrawalRequest>,
    pub nonpayable: Vec<NonPayableWithdrawal>,
}

fn deserialize_address<'de, D>(deserializer: D) -> Result<BitcoinAddress, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // The stored snapshot was network-checked at import; signing checks its network domain again.
    let address =
        bitcoin::Address::<bitcoin::address::NetworkUnchecked>::deserialize(deserializer)?;
    Ok(address.assume_checked())
}
