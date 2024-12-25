use std::{clone::Clone, collections::HashMap, sync::Arc};

use bitcoin::Address;
use musig2::{CompactSignature, PartialSignature, PubNonce};
use secp256k1_musig2::PublicKey;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use via_btc_client::withdrawal_builder::UnsignedWithdrawalTx;
use via_musig2::Signer;

#[derive(Clone)]
pub struct ViaWithdrawalState {
    pub signer: Arc<RwLock<Signer>>,
    pub signing_session: Arc<RwLock<SigningSession>>,
    pub unsigned_tx: Arc<RwLock<Option<UnsignedWithdrawalTx>>>,
    pub bridge_address: Address,
    pub required_signers: usize,
}

#[derive(Default, Debug, Clone)]
pub struct SigningSession {
    pub initialized: bool,
    pub block_number: u64,
    pub tx_id: String,
    pub received_nonces: HashMap<usize, PubNonce>,
    pub received_sigs: HashMap<usize, PartialSignature>,
    pub final_signature: Option<CompactSignature>,
    pub message: Vec<u8>,
    pub finished: bool,
}

/// Data posted by other signers to submit their nonce
#[derive(Serialize, Deserialize, Debug)]
pub struct NoncePair {
    pub signer_index: usize,
    pub nonce: String, // Base64 encoded
}

/// Data posted by other signers to submit their partial signature
#[derive(Serialize, Deserialize, Debug)]
pub struct PartialSignaturePair {
    pub signer_index: usize,
    pub signature: String, // Base64 encoded
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SigningSessionResponse {
    pub block_number: u64,
    pub message_to_sign: String,   // hex-encoded message (txid)
    pub aggregated_pubkey: String, // hex-encoded aggregated pubkey
    pub required_signers: usize,
    pub received_nonces: usize,
    pub received_partial_signatures: usize,
    pub final_signature: Option<String>, // hex-encoded final signature if present
}
