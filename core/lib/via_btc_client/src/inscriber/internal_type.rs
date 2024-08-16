use bitcoin::{taproot::ControlBlock, Amount, Transaction, TxIn, TxOut, Txid};

pub struct CommitTxInputRes {
    pub commit_tx_inputs: Vec<TxIn>,
    pub unlocked_value: Amount,
    pub inputs_count: u32,
    pub utxo_amounts: Vec<Amount>,
}

pub struct CommitTxOutputRes {
    pub commit_tx_change_output: TxOut,
    pub commit_tx_tapscript_output: TxOut,
    pub commit_tx_fee_rate: u64,
    pub _commit_tx_fee: Amount,
}

pub struct RevealTxInputRes {
    pub reveal_tx_input: Vec<TxIn>,
    pub prev_outs: Vec<TxOut>,
    pub unlock_value: Amount,
    pub control_block: ControlBlock,
}

pub struct RevealTxOutputRes {
    pub reveal_tx_change_output: TxOut,
    pub reveal_fee_rate: u64,
    pub _reveal_fee: Amount,
}

pub struct FinalTx {
    pub tx: Transaction,
    pub txid: Txid,
}
