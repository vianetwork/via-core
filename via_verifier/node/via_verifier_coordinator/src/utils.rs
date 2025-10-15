use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use base64::Engine;
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

pub(crate) fn seconds_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Incorrect system time")
        .as_secs()
}
