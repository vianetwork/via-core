use bitcoin::{Address, Amount};

#[derive(Debug)]
pub struct WithdrawalRequest {
    pub address: Address,
    pub amount: Amount,
}
