#[derive(Debug, Clone)]
pub struct Deposit {
    pub priority_id: i64,
    pub tx_id: Vec<u8>,
    pub block_number: u32,
    pub sender: String,
    pub receiver: String,
    pub value: i64,
    pub calldata: Vec<u8>,
    pub canonical_tx_hash: Vec<u8>,
    pub block_timestamp: u64,
}
