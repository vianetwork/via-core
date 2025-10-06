#[derive(Debug, Clone)]
pub struct Withdrawal {
    pub id: String,
    pub tx_id: Vec<u8>,
    pub l2_tx_log_index: i64,
    pub block_number: i64,
    pub receiver: String,
    pub value: i64,
    pub timestamp: i64,
}
