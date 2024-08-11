pub use bitcoin::script::PushBytesBuf;
pub use bitcoin::taproot::Signature as TaprootSignature;
pub use bitcoin::Address as BitcoinAddress;
use bitcoin::Amount;
use bitcoin::TxIn;
pub use bitcoin::Txid;

use zksync_basic_types::H256;
use zksync_da_client::types::DispatchResponse;
use zksync_types::Address as EVMAddress;
use zksync_types::L1BatchNumber;

use std::collections::VecDeque;

// Enum for Message Type
pub enum MessageType {
    L1BatchDAReference,
    ProofDAReferenceMessage,
    ValidatorAttestationMessage,
    SystemBootstrappingMessage,
    ProposeSequencerMessage,
    L1ToL2Message,
}

#[derive(Clone)]
pub enum Vote {
    Ok,    // OP_1
    NotOk, // OP_0
}

/*
    FINAL STRUCTURES
*/
// Common structure for Schnorr Signature, Encoded Public Key, via_inscription_protocol, and message_type
pub struct CommonFields {
    schnorr_signature: TaprootSignature,
    encoded_public_key: PushBytesBuf,
    via_inscription_protocol_identifier: String,
    message_type: MessageType,
}

// L1BatchDAReference message
// We use DispatchResponse as type for da_reference
// It's hex string with following structure =>[8]byte da chain block height ++ [32]byte commitment
pub struct L1BatchDAReference {
    common: CommonFields,
    l1_batch_hash: H256,
    l1_batch_index: L1BatchNumber,
    da_identifier: String,
    da_reference: DispatchResponse,
}

// ProofDAReferenceMessage message
pub struct ProofDAReferenceMessage {
    common: CommonFields,
    l1_batch_reveal_txid: Txid,
    da_identifier: String,
    da_reference: DispatchResponse,
}

// ValidatorAttestationMessage message
pub struct ValidatorAttestationMessage {
    common: CommonFields,
    reference_txid: Txid,
    attestation: Vote,
}

// SystemBootstrappingMessage message
pub struct SystemBootstrappingMessage {
    common: CommonFields,
    start_block_height: u32, // this type is community standard for bitcoin block height
    verifier_addresses: Vec<BitcoinAddress>,
    bridge_p2wpkh_mpc_address: BitcoinAddress,
}

// ProposeSequencerMessage message
pub struct ProposeSequencerMessage {
    common: CommonFields,
    sequencer_p2wpkh_address: BitcoinAddress,
}

// L1ToL2Message message
pub struct L1ToL2Message {
    common: CommonFields,
    receiver_l2_address: EVMAddress,
    l2_contract_address: EVMAddress,
    call_data: Vec<u8>, // this is the community standard type for calldata
}

/*
    INPUT
*/

#[derive(Clone)]
pub enum InscriberInput {
    L1BatchDAReference {
        l1_batch_hash: H256,
        l1_batch_index: L1BatchNumber,
        da_reference: DispatchResponse,
    },
    ProofDAReference {
        l1_batch_reveal_txid: Txid,
        da_reference: DispatchResponse,
    },
    ValidatorAttestation {
        reference_txid: Txid,
        vote: Vote,
    },
    SystemBootstrapping {
        start_block_height: u32,
        verifier_p2wpkh_addresses: Vec<BitcoinAddress>,
        bridge_p2wpkh_mpc_address: BitcoinAddress,
    },
    ProposeSequencer {
        sequencer_new_p2wpkh_address: BitcoinAddress,
    },
    L1ToL2Message {
        receiver_l2_address: EVMAddress,
        l2_contract_address: EVMAddress,
        call_data: Vec<u8>,
    },
}

#[derive(Clone)]
pub struct FeePayerCtx {
    pub fee_payer_utxo_txid: Txid,
    pub fee_payer_utxo_vout: u32, // this is the type bitcoin rust also uses for vout
    pub fee_payer_utxo_value: Amount,
}

#[derive(Clone)]
pub struct CommitTxInput {
    pub spent_utxo: Vec<TxIn>,
}

#[derive(Clone)]
pub struct InscriptionRequest {
    pub _message: InscriberInput,
    pub inscriber_output: InscriberOutput,
    pub fee_payer_ctx: FeePayerCtx,
    pub commit_tx_input: CommitTxInput,
}

// this context should get persisted in the database in the upper layer
// and also the update method checks the transaction is confirmed or not
// if the transaction that tx should remove from the context.
// the inscribe method first calls update context method before inscribing the message
// the upper layer after calling inscribe method should persist the context in the database

#[derive(Clone)]
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

/*
    OUTPUT
*/

#[derive(Clone)]
pub struct InscriberOutput {
    pub commit_txid: Txid,
    pub commit_raw_tx: String, // this is the type bitcoin rust also uses for raw tx
    pub commit_tx_fee_rate: u64,
    pub reveal_txid: Txid,
    pub reveal_raw_tx: String, // this is the type bitcoin rust also uses for raw tx
    pub reveal_tx_fee_rate: u64,
    pub is_broadcasted: bool,
}
