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
    Withdrawal(i64, Vec<UnsignedBridgeTx>, Vec<Vec<u8>>, Vec<u8>, usize),
}

impl SessionOperation {
    pub fn get_l1_batch_number(&self) -> i64 {
        match self {
            Self::Withdrawal(l1_batch_number, _, _, _, _) => *l1_batch_number,
        }
    }

    pub fn get_session_type(&self) -> SessionType {
        match self {
            Self::Withdrawal(_, _, _, _, _) => SessionType::Withdrawal,
        }
    }

    pub fn get_message_to_sign(&self) -> Vec<Vec<u8>> {
        match self {
            Self::Withdrawal(_, _, message, _, _) => message.clone(),
        }
    }

    pub fn get_unsigned_bridge_tx(&self) -> UnsignedBridgeTx {
        match self {
            Self::Withdrawal(_, unsigned_txs, _, _, index) => {
                return unsigned_txs[index.clone()].clone();
            }
        }
    }

    pub fn get_proof_tx_id(&self) -> Vec<u8> {
        match self {
            Self::Withdrawal(_, _, _, proof_tx_id, _) => proof_tx_id.clone(),
        }
    }

    pub fn session(&self) -> Option<(UnsignedBridgeTx, &Vec<Vec<u8>>)> {
        match self {
            Self::Withdrawal(_, unsigned_txs, message, _, index) => {
                Some((unsigned_txs[index.clone()].clone(), message))
            }
        }
    }

    pub fn unsigned_txs(&self) -> &Vec<UnsignedBridgeTx> {
        match self {
            Self::Withdrawal(_, unsigned_txs, _, _, _) => unsigned_txs,
        }
    }

    pub fn index(&self) -> usize {
        match self {
            Self::Withdrawal(_, _, _, _, index) => *index,
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
    pub verifier_request_timeout: u8,
    pub session_timeout: u64,
}

#[derive(Default, Debug, Clone)]
pub struct SigningSession {
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
    pub session_op: Vec<u8>,
    pub required_signers: usize,
    pub received_nonces: BTreeMap<usize, usize>,
    pub received_partial_signatures: BTreeMap<usize, usize>,
    pub created_at: u64,
}
