use bitcoin::{Amount, OutPoint, TxOut};

#[derive(Debug, Clone)]
pub struct TransactionMetadata {
    pub outputs: Vec<TxOut>,
    pub inputs: Vec<(OutPoint, TxOut)>,
    pub total_amount: Amount,
    pub fee: Amount,
}
