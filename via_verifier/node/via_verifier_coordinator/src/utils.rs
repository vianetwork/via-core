use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use base64::Engine;
use bitcoin::{hashes::Hash, Txid};
use musig2::{BinaryEncoding, PartialSignature, PubNonce};

use crate::types::{NoncePair, PartialSignaturePair};

pub fn decode_signature(signature: String) -> anyhow::Result<PartialSignature> {
    let decoded_sig = base64::engine::general_purpose::STANDARD
        .decode(&signature)
        .with_context(|| "Error to decode signature")?;
    Ok(PartialSignature::from_slice(&decoded_sig)?)
}

pub fn encode_signature(
    signer_index: usize,
    partial_sig: PartialSignature,
) -> anyhow::Result<PartialSignaturePair> {
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(partial_sig.serialize());
    let sig_pair = PartialSignaturePair {
        signer_index,
        signature: sig_b64,
    };
    Ok(sig_pair)
}

pub fn encode_nonce(signer_index: usize, nonce: PubNonce) -> anyhow::Result<NoncePair> {
    let nonce = base64::engine::general_purpose::STANDARD.encode(nonce.to_bytes());
    Ok(NoncePair {
        signer_index,
        nonce,
    })
}

pub fn decode_nonce(nonce_pair: NoncePair) -> anyhow::Result<PubNonce> {
    let decoded_nonce = base64::engine::general_purpose::STANDARD
        .decode(&nonce_pair.nonce)
        .with_context(|| "Error to encode nonce")?;
    let pub_nonce = PubNonce::from_bytes(&decoded_nonce)?;
    Ok(pub_nonce)
}

/// Converts H256 bytes (from the DB) to a Txid by reversing the byte order.
pub(crate) fn h256_to_txid(h256_bytes: &[u8]) -> anyhow::Result<Txid> {
    if h256_bytes.len() != 32 {
        return Err(anyhow::anyhow!("H256 must be 32 bytes"));
    }
    let mut reversed_bytes = h256_bytes.to_vec();
    reversed_bytes.reverse();
    Txid::from_slice(&reversed_bytes).with_context(|| "Failed to convert H256 to Txid")
}

pub(crate) fn seconds_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Incorrect system time")
        .as_secs()
}
