#[derive(Debug, Clone)]
pub struct Withdrawal {
    pub l2_tx_hash: String,
    pub l2_tx_index: i64,
    pub receiver: String,
    pub value: i64,
}
