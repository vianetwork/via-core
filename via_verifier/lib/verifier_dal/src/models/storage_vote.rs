#[derive(Debug, Clone)]
pub struct CanonicalChainStatus {
    pub is_valid: bool,
    pub total_canonical_batches: i64,
    pub max_batch_number: Option<u32>,
    pub min_batch_number: Option<u32>,
    pub missing_batches: Vec<u32>,
    pub batch_sequence: Vec<u32>,
    pub total_transactions_in_db: i64,
    pub has_genesis: bool,
}
