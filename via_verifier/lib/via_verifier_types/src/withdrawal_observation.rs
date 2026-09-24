use bitcoin::{Amount, BlockHash, OutPoint, ScriptBuf, Transaction, TxOut};
use serde::{Deserialize, Serialize};

/// Payment evidence is independent of expected obligations and survives their invalidation.
/// BIP341 (Common signature message) commits to every prevout's amount and script;
/// retain those verified bytes, rather than reconstructing authority from payment outputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WithdrawalObservation {
    pub transaction: Transaction,
    pub prevouts: Vec<(OutPoint, TxOut)>,
    pub withdrawals: Vec<ObservedWithdrawal>,
    pub inclusion: Option<WithdrawalInclusion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedWithdrawal {
    pub vout: u32,
    pub reference: String,
    pub script_pubkey: ScriptBuf,
    pub amount: Amount,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WithdrawalInclusion {
    pub block_hash: BlockHash,
    pub block_height: u32,
}
