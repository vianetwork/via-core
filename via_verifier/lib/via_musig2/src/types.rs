use std::sync::Arc;

use bitcoin::{Address, Amount, OutPoint, TxOut};

use crate::fee::FeeStrategy;

#[derive(Debug, Clone)]
pub struct TransactionMetadata {
    pub outputs: Vec<TransactionOutput>,
    pub inputs: Vec<(OutPoint, TxOut)>,
    pub total_amount: Amount,
    pub fee: Amount,
    pub fee_rate: u64,
}

#[derive(Debug, Clone)]
pub struct TransactionOutput {
    pub output: TxOut,
    /// The output metadata to store in OP_RETURN
    pub op_return_data: Option<Vec<u8>>,
}

#[derive(Clone)]
pub struct TransactionBuilderConfig {
    pub fee_strategy: Arc<dyn FeeStrategy>,
    /// The max tx weight
    pub max_tx_weight: u64,
    /// The max number of output to include in each transaction
    pub max_output_per_tx: usize,
    pub op_return_prefix: Vec<u8>,
    pub bridge_address: Address,
    /// The fee rate.
    pub default_fee_rate_opt: Option<u64>,
    /// Fixed input candidates instead of querying the wallet.
    pub default_available_utxos_opt: Option<Vec<(OutPoint, TxOut)>>,
    /// Shared metadata inscribed before the per-output metadata.
    pub op_return_data_input_opt: Option<Vec<u8>>,
}

impl TransactionBuilderConfig {
    pub fn withdrawal(bridge_address: Address) -> Self {
        Self {
            fee_strategy: Arc::new(crate::fee::WithdrawalFeeStrategy::new()),
            max_tx_weight: bitcoin::policy::MAX_STANDARD_TX_WEIGHT as u64,
            max_output_per_tx: 7,
            op_return_prefix: b"VIA_WI\0".to_vec(),
            bridge_address,
            default_fee_rate_opt: None,
            default_available_utxos_opt: None,
            op_return_data_input_opt: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct TransactionWithFee {
    /// The transaction output - fees
    pub outputs_with_fees: Vec<TransactionOutput>,
    /// The fee per user
    pub fee: Amount,
    /// The total value requests
    pub total_value_needed: Amount,
}
