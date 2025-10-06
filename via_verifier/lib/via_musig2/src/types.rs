use std::sync::Arc;

use bitcoin::{Address, Amount, OutPoint, TxOut};

use crate::fee::FeeStrategy;

#[derive(Debug, Clone)]
pub struct TransactionMetadata {
    pub outputs: Vec<TransactionOutput>,
    /// The data inscribed into op_return per output. Can be empty.
    // pub op_return_data_per_output: Vec<Vec<u8>>,
    pub inputs: Vec<(OutPoint, TxOut)>,
    pub total_amount: Amount,
    pub fee: Amount,
    pub fee_rate: u64,
}

#[derive(Debug, Clone)]
pub struct TransactionOutput {
    /// The output
    pub output: TxOut,
    /// The output metadata to store in OP_RETURN
    pub op_return_data: Option<Vec<u8>>,
}

#[derive(Clone)]
pub struct TransactionBuilderConfig {
    /// The fee strategy
    pub fee_strategy: Arc<dyn FeeStrategy>,
    /// The max tx weight
    pub max_tx_weight: u64,
    /// The max number of output to include in each transaction
    pub max_output_per_tx: usize,
    /// The OP_RETURN prefix
    pub op_return_prefix: Vec<u8>,
    /// Bridge address
    pub bridge_address: Address,
    /// The fee rate.
    pub default_fee_rate_opt: Option<u64>,
    /// The transaction fee rate
    pub default_available_utxos_opt: Option<Vec<(OutPoint, TxOut)>>,
    /// Unique data thta will be inscribed in all the bridge txs
    pub op_return_data_input_opt: Option<Vec<u8>>,
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
