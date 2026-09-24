use std::{clone::Clone, collections::BTreeMap, fmt, sync::Arc};

use bincode::{deserialize, serialize};
use musig2::{PartialSignature, PubNonce};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use via_btc_client::traits::Serializable;
use via_verifier_types::transaction::UnsignedBridgeTx;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionType {
    Withdrawal,
}

impl fmt::Display for SessionType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                SessionType::Withdrawal => "Withdrawal",
            }
        )
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionOperation {
    Withdrawal(UnsignedBridgeTx, Vec<Vec<u8>>, Vec<bitcoin::Transaction>),
}

impl SessionOperation {
    pub fn get_session_type(&self) -> SessionType {
        match self {
            Self::Withdrawal(_, _, _) => SessionType::Withdrawal,
        }
    }

    pub fn get_message_to_sign(&self) -> Vec<Vec<u8>> {
        match self {
            Self::Withdrawal(_, message, _) => message.clone(),
        }
    }

    pub fn get_unsigned_bridge_tx(&self) -> UnsignedBridgeTx {
        match self {
            Self::Withdrawal(unsigned_tx, _, _) => {
                return unsigned_tx.clone();
            }
        }
    }

    pub fn get_parent_transactions(&self) -> &[bitcoin::Transaction] {
        match self {
            Self::Withdrawal(_, _, parents) => parents,
        }
    }
}

impl Serializable for SessionOperation {
    fn to_bytes(&self) -> Vec<u8> {
        serialize(self).expect("error serialize the SessionOperation")
    }

    fn from_bytes(bytes: &[u8]) -> Self
    where
        Self: Sized,
    {
        deserialize(bytes).expect("error deserialize the SessionOperation")
    }
}

#[derive(Debug, Clone)]
pub struct ViaWithdrawalState {
    pub signing_session: Arc<RwLock<SigningSession>>,
    pub verifiers_pub_keys: Vec<bitcoin::secp256k1::PublicKey>,
}

#[derive(Default, Debug, Clone)]
pub struct SigningSession {
    pub round_id: [u8; 32],
    pub content_hash: [u8; 32],
    pub authorized_content: Vec<u8>,
    pub session_op: Option<SessionOperation>,
    pub received_nonces: BTreeMap<usize, BTreeMap<usize, PubNonce>>,
    pub received_sigs: BTreeMap<usize, BTreeMap<usize, PartialSignature>>,
    pub created_at: u64,
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
    pub round_id: [u8; 32],
    pub content_hash: [u8; 32],
    pub authorized_content: Vec<u8>,
    pub nonces: BTreeMap<usize, BTreeMap<usize, String>>,
    pub signatures: BTreeMap<usize, BTreeMap<usize, String>>,
    pub session_op: Vec<u8>,
    pub required_signers: usize,
    pub received_nonces: BTreeMap<usize, usize>,
    pub received_partial_signatures: BTreeMap<usize, usize>,
    pub created_at: u64,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct AdmittedContent {
    pub proposal: Vec<u8>,
    pub network: bitcoin::Network,
    pub chain_id: u64,
    pub wallet: Vec<u8>,
    pub participants: Vec<String>,
    pub tweak: Option<String>,
    pub expected: Vec<via_verifier_types::withdrawal::WithdrawalRequest>,
}
