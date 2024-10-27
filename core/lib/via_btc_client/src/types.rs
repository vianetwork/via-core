use std::collections::VecDeque;

use bincode::{deserialize, serialize};
use bitcoin::{
    address::NetworkUnchecked, script::PushBytesBuf, taproot::Signature as TaprootSignature,
    Amount, TxIn, TxOut, Txid,
};
pub use bitcoin::{Address as BitcoinAddress, Network as BitcoinNetwork, Txid as BitcoinTxid};
pub use bitcoincore_rpc::Auth as NodeAuth;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zksync_basic_types::H256;
use zksync_object_store::{serialize_using_bincode, Bucket, StoredObject};
use zksync_types::{Address as EVMAddress, L1BatchNumber};

use crate::traits::Serializable;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Vote {
    Ok,
    NotOk,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
pub struct CommonFields {
    pub schnorr_signature: TaprootSignature,
    pub encoded_public_key: PushBytesBuf,
    pub block_height: u32,
    pub tx_id: Txid,
    pub p2wpkh_address: BitcoinAddress,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemBootstrappingInput {
    pub start_block_height: u32,
    pub verifier_p2wpkh_addresses: Vec<BitcoinAddress<NetworkUnchecked>>,
    pub bridge_p2wpkh_mpc_address: BitcoinAddress<NetworkUnchecked>,
    pub bootloader_hash: H256,
    pub abstract_account_hash: H256,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemBootstrapping {
    pub common: CommonFields,
    pub input: SystemBootstrappingInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProposeSequencerInput {
    pub sequencer_new_p2wpkh_address: BitcoinAddress<NetworkUnchecked>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProposeSequencer {
    pub common: CommonFields,
    pub input: ProposeSequencerInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    pub tx_outputs: Vec<TxOut>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum InscriptionMessage {
    L1BatchDAReference(L1BatchDAReferenceInput),
    ProofDAReference(ProofDAReferenceInput),
    ValidatorAttestation(ValidatorAttestationInput),
    SystemBootstrapping(SystemBootstrappingInput),
    ProposeSequencer(ProposeSequencerInput),
    L1ToL2Message(L1ToL2MessageInput),
}

impl Serializable for InscriptionMessage {
    fn to_bytes(&self) -> Vec<u8> {
        serialize(self).expect("error serialize the InscriptionMessage")
    }

    fn from_bytes(bytes: &[u8]) -> Self
    where
        Self: Sized,
    {
        deserialize(bytes).expect("error deserialize the InscriptionMessage")
    }
}

#[derive(Debug)]
pub struct InscriptionConfig {
    pub fee_multiplier: u64,
}

impl Default for InscriptionConfig {
    fn default() -> Self {
        InscriptionConfig { fee_multiplier: 1 }
    }
}

#[derive(Debug)]
pub struct Recipient {
    pub address: BitcoinAddress,
    pub amount: Amount,
}

impl Recipient {
    pub fn new(address: BitcoinAddress, amount: Amount) -> Self {
        Recipient { address, amount }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FullInscriptionMessage {
    L1BatchDAReference(L1BatchDAReference),
    ProofDAReference(ProofDAReference),
    ValidatorAttestation(ValidatorAttestation),
    SystemBootstrapping(SystemBootstrapping),
    ProposeSequencer(ProposeSequencer),
    L1ToL2Message(L1ToL2Message),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeePayerCtx {
    pub fee_payer_utxo_txid: Txid,
    pub fee_payer_utxo_vout: u32,
    pub fee_payer_utxo_value: Amount,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommitTxInput {
    pub spent_utxo: Vec<TxIn>,
}

lazy_static! {
    pub static ref SYSTEM_BOOTSTRAPPING_MSG: PushBytesBuf =
        PushBytesBuf::from(b"SystemBootstrappingMessage");
    pub static ref PROPOSE_SEQUENCER_MSG: PushBytesBuf =
        PushBytesBuf::from(b"ProposeSequencerMessage");
    pub static ref VALIDATOR_ATTESTATION_MSG: PushBytesBuf =
        PushBytesBuf::from(b"ValidatorAttestationMessage");
    pub static ref L1_BATCH_DA_REFERENCE_MSG: PushBytesBuf =
        PushBytesBuf::from(b"L1BatchDAReferenceMessage");
    pub static ref PROOF_DA_REFERENCE_MSG: PushBytesBuf =
        PushBytesBuf::from(b"ProofDAReferenceMessage");
    pub static ref L1_TO_L2_MSG: PushBytesBuf = PushBytesBuf::from(b"L1ToL2Message");
}
pub(crate) const VIA_INSCRIPTION_PROTOCOL: &str = "via_inscription_protocol";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InscriptionRequest {
    pub message: InscriptionMessage,
    pub inscriber_output: InscriberOutput,
    pub fee_payer_ctx: FeePayerCtx,
    pub commit_tx_input: CommitTxInput,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InscriberContext {
    pub fifo_queue: VecDeque<InscriptionRequest>,
}

const CTX_CAPACITY: usize = 10;

impl InscriberContext {
    pub fn new() -> Self {
        Self {
            fifo_queue: VecDeque::with_capacity(CTX_CAPACITY),
        }
    }
}

impl Default for InscriberContext {
    fn default() -> Self {
        Self::new()
    }
}

impl StoredObject for InscriberContext {
    const BUCKET: Bucket = Bucket::ViaInscriberContext;

    type Key<'a> = u32;

    fn encode_key(key: Self::Key<'_>) -> String {
        format!("inscriber_context_{key}.bin")
    }

    serialize_using_bincode!();
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InscriberOutput {
    pub commit_txid: Txid,
    pub commit_raw_tx: String,
    pub commit_tx_fee_rate: u64,
    pub reveal_txid: Txid,
    pub reveal_raw_tx: String,
    pub reveal_tx_fee_rate: u64,
    pub is_broadcasted: bool,
}

#[derive(Debug, Error, Clone)]
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

/// Custom error type for the BitcoinInscriptionIndexer
#[derive(Error, Debug)]
pub enum IndexerError {
    #[error("Bootstrap process incomplete: {0}")]
    IncompleteBootstrap(String),
    #[error("Invalid block height: {0}")]
    InvalidBlockHeight(u32),
    #[error("Bitcoin client error: {0}")]
    BitcoinClientError(#[from] BitcoinError),
}

pub type BitcoinIndexerResult<T> = std::result::Result<T, IndexerError>;
pub type BitcoinSignerResult<T> = Result<T>;
pub type BitcoinInscriberResult<T> = Result<T>;

pub type BitcoinTransactionBuilderResult<T> = Result<T>;
