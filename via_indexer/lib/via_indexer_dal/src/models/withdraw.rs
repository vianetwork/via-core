#[derive(Debug, Clone)]
pub struct BridgeWithdrawalParam {
    pub tx_id: Vec<u8>,
    pub l1_batch_reveal_tx_id: Vec<u8>,
    pub block_number: i64,
    pub fee: i64,
    pub vsize: i64,
    pub total_size: i64,
    pub withdrawals_count: i64,
}

#[derive(Debug, Clone)]
pub struct WithdrawalParam {
    pub tx_index: i64,
    pub receiver: String,
    pub value: i64,
}
