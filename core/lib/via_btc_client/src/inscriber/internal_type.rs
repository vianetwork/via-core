use bitcoin::{taproot::ControlBlock, Amount, Transaction, TxIn, TxOut, Txid};

#[derive(Debug)]
pub struct CommitTxInputRes {
    pub commit_tx_inputs: Vec<TxIn>,
    pub unlocked_value: Amount,
    pub inputs_count: u32,
    pub utxo_amounts: Vec<Amount>,
}

#[derive(Debug)]
pub struct CommitTxOutputRes {
    pub commit_tx_change_output: TxOut,
    pub commit_tx_tapscript_output: TxOut,
    pub commit_tx_fee_rate: u64,
    pub _commit_tx_fee: Amount,
}

#[derive(Debug)]
pub struct RevealTxInputRes {
    pub reveal_tx_input: Vec<TxIn>,
    pub prev_outs: Vec<TxOut>,
    pub unlock_value: Amount,
    pub control_block: ControlBlock,
}

#[derive(Debug)]
pub struct RevealTxOutputRes {
    pub reveal_tx_change_output: TxOut,
    pub recipient_tx_output: Option<TxOut>,
    pub reveal_fee_rate: u64,
    pub _reveal_fee: Amount,
}

#[derive(Debug)]
pub struct FinalTx {
    pub tx: Transaction,
    pub txid: Txid,
}

#[derive(Debug)]
pub struct InscriberInfo {
    pub final_commit_tx: FinalTx,
    pub final_reveal_tx: FinalTx,
    pub commit_tx_output_info: CommitTxOutputRes,
    pub reveal_tx_output_info: RevealTxOutputRes,
    pub commit_tx_input_info: CommitTxInputRes,
}
