use std::collections::VecDeque;

use bitcoin::{
    script::PushBytesBuf, taproot::Signature as TaprootSignature, Address as BitcoinAddress,
    Amount, TxIn, Txid,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zksync_basic_types::H256;
use zksync_types::{Address as EVMAddress, L1BatchNumber};

#[derive(Serialize, Deserialize)]
pub enum BitcoinMessage {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Vote {
    Ok,
    NotOk,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommonFields {
    pub schnorr_signature: TaprootSignature,
    pub encoded_public_key: PushBytesBuf,
    pub via_inscription_protocol_identifier: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct L1BatchDAReferenceInput {
    pub l1_batch_hash: H256,
    pub l1_batch_index: L1BatchNumber,
    pub da_identifier: String,
    pub blob_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct L1BatchDAReference {
    pub common: CommonFields,
    pub input: L1BatchDAReferenceInput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProofDAReferenceInput {
    pub l1_batch_reveal_txid: Txid,
    pub da_identifier: String,
    pub blob_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProofDAReference {
    pub common: CommonFields,
    pub input: ProofDAReferenceInput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidatorAttestationInput {
    pub reference_txid: Txid,
    pub attestation: Vote,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidatorAttestation {
    pub common: CommonFields,
    pub input: ValidatorAttestationInput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemBootstrappingInput {
    pub start_block_height: u32,
    pub verifier_p2wpkh_addresses: Vec<BitcoinAddress>,
    pub bridge_p2wpkh_mpc_address: BitcoinAddress,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemBootstrapping {
    pub common: CommonFields,
    pub input: SystemBootstrappingInput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProposeSequencerInput {
    pub sequencer_new_p2wpkh_address: BitcoinAddress,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProposeSequencer {
    pub common: CommonFields,
    pub input: ProposeSequencerInput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct L1ToL2MessageInput {
    pub receiver_l2_address: EVMAddress,
    pub l2_contract_address: EVMAddress,
    pub call_data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct L1ToL2Message {
    pub common: CommonFields,
    pub amount: Amount,
    pub input: L1ToL2MessageInput,
}

#[allow(unused)]
#[derive(Clone, Debug, PartialEq)]
pub enum InscriptionMessage {
    L1BatchDAReference(L1BatchDAReferenceInput),
    ProofDAReference(ProofDAReferenceInput),
    ValidatorAttestation(ValidatorAttestationInput),
    SystemBootstrapping(SystemBootstrappingInput),
    ProposeSequencer(ProposeSequencerInput),
    L1ToL2Message(L1ToL2MessageInput),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FeePayerCtx {
    pub fee_payer_utxo_txid: Txid,
    pub fee_payer_utxo_vout: u32,
    pub fee_payer_utxo_value: Amount,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommitTxInput {
    pub spent_utxo: Vec<TxIn>,
}

#[allow(unused)]
#[derive(Clone, Debug)]
pub struct InscriptionRequest {
    pub message: InscriptionMessage,
    pub inscriber_output: InscriberOutput,
    pub fee_payer_ctx: FeePayerCtx,
    pub commit_tx_input: CommitTxInput,
}

#[allow(unused)]
#[derive(Clone, Debug)]
pub struct InscriberContext {
    pub fifo_queue: VecDeque<InscriptionRequest>,
}

#[allow(unused)]
const CTX_CAPACITY: usize = 10;

#[allow(unused)]
impl InscriberContext {
    pub fn new() -> Self {
        Self {
            fifo_queue: VecDeque::with_capacity(CTX_CAPACITY),
        }
    }
}

#[allow(unused)]
#[derive(Clone, Debug)]
pub struct InscriberOutput {
    pub commit_txid: Txid,
    pub commit_raw_tx: String,
    pub commit_tx_fee_rate: u64,
    pub reveal_txid: Txid,
    pub reveal_raw_tx: String,
    pub reveal_tx_fee_rate: u64,
    pub is_broadcasted: bool,
}

#[derive(Debug, Error)]
pub enum BitcoinError {
    #[error("RPC error: {0}")]
    Rpc(String),

    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    #[error("Invalid transaction: {0}")]
    InvalidTransaction(String),

    #[error("Signing error: {0}")]
    SigningError(String),

    #[error("Inscription error: {0}")]
    InscriptionError(String),

    #[error("Indexing error: {0}")]
    IndexingError(String),

    #[error("Transaction building error: {0}")]
    TransactionBuildingError(String),

    #[error("Fee estimation error: {0}")]
    FeeEstimationFailed(String),

    #[error("Invalid network: {0}")]
    InvalidNetwork(String),

    #[error("Invalid output point: {0}")]
    InvalidOutpoint(String),

    #[error("Invalid private key: {0}")]
    InvalidPrivateKey(String),

    #[error("Compressed public key error: {0}")]
    CompressedPublicKeyError(String),

    #[error("Uncompressed public key error: {0}")]
    UncompressedPublicKeyError(String),

    #[error("Other error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, BitcoinError>;

pub type BitcoinClientResult<T> = Result<T>;
pub type BitcoinRpcResult<T> = Result<T>;

impl From<bitcoincore_rpc::Error> for BitcoinError {
    fn from(error: bitcoincore_rpc::Error) -> Self {
        BitcoinError::Rpc(error.to_string())
    }
}

impl From<bitcoin::address::ParseError> for BitcoinError {
    fn from(error: bitcoin::address::ParseError) -> Self {
        BitcoinError::InvalidAddress(error.to_string())
    }
}

impl From<bitcoin::hex::HexToArrayError> for BitcoinError {
    fn from(error: bitcoin::hex::HexToArrayError) -> Self {
        BitcoinError::InvalidTransaction(error.to_string())
    }
}

pub type BitcoinSignerResult<T> = Result<T>;
pub type BitcoinInscriberResult<T> = Result<T>;
pub type BitcoinIndexerResult<T> = Result<T>;
#[allow(unused)]
pub type BitcoinTransactionBuilderResult<T> = Result<T>;
