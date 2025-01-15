use std::{clone::Clone, collections::HashMap, sync::Arc};

use musig2::{PartialSignature, PubNonce};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use via_btc_client::withdrawal_builder::UnsignedWithdrawalTx;

#[derive(Debug, Clone)]
pub struct ViaWithdrawalState {
    pub signing_session: Arc<RwLock<SigningSession>>,
    pub required_signers: usize,
}

#[derive(Default, Debug, Clone)]
pub struct SigningSession {
    pub l1_block_number: i64,
    pub unsigned_tx: Option<UnsignedWithdrawalTx>,
    pub received_nonces: HashMap<usize, PubNonce>,
    pub received_sigs: HashMap<usize, PartialSignature>,
    pub message: Vec<u8>,
}

/// Data posted by other signers to submit their nonce
#[derive(Serialize, Deserialize, Debug)]
pub struct NoncePair {
    pub signer_index: usize,
    /// Base64 encoded signer nonce
    pub nonce: String,
}

/// Data posted by other signers to submit their partial signature
#[derive(Serialize, Deserialize, Debug)]
pub struct PartialSignaturePair {
    pub signer_index: usize,
    /// Base64 encoded signature
    pub signature: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SigningSessionResponse {
    pub l1_block_number: i64,
    /// hex-encoded message (txid)
    pub message_to_sign: String,
    pub required_signers: usize,
    pub unsigned_tx: Vec<u8>,
    pub received_nonces: usize,
    pub received_partial_signatures: usize,
}
