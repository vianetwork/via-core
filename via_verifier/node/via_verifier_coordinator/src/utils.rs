use std::str::FromStr;

use anyhow::Context;
use base64::Engine;
use bitcoin::{hashes::Hash, PrivateKey, Txid};
use musig2::{BinaryEncoding, PartialSignature, PubNonce};
use secp256k1_musig2::{PublicKey, Secp256k1, SecretKey};
use via_musig2::Signer;

use crate::types::{NoncePair, PartialSignaturePair};

pub fn get_signer(
    private_key_wif: &str,
    verifiers_pub_keys_str: Vec<String>,
) -> anyhow::Result<Signer> {
    let private_key = PrivateKey::from_wif(private_key_wif)?;
    let secret_key = SecretKey::from_byte_array(&private_key.inner.secret_bytes())
        .with_context(|| "Error to compute the coordinator sk")?;
    let secp = Secp256k1::new();
    let public_key = PublicKey::from_secret_key(&secp, &secret_key);

    let mut all_pubkeys = Vec::new();

    let mut signer_index = 0;

    for (i, key) in verifiers_pub_keys_str.iter().enumerate() {
        let pk = PublicKey::from_str(key)?;
        all_pubkeys.push(pk);
        if pk == public_key {
            signer_index = i;
        }
    }

    let signer = Signer::new(secret_key, signer_index, all_pubkeys.clone())?;
    Ok(signer)
}

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
