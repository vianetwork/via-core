use std::{clone::Clone, str::FromStr};

use anyhow::Context;
use base64::Engine;
use bitcoin::PrivateKey;
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
        .context("Error to compute the coordinator sk")?;
    let secp = Secp256k1::new();
    let public_key = PublicKey::from_secret_key(&secp, &secret_key);

    let mut all_pubkeys = Vec::new();

    let mut signer_index = 0;

    for (i, key) in verifiers_pub_keys_str.iter().enumerate() {
        let pk = PublicKey::from_slice(key.as_bytes())?;
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
        .context("error to decode signature")?;
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
        .context("error to encode nonde")?;
    let pub_nonce = PubNonce::from_bytes(&decoded_nonce)?;
    Ok(pub_nonce)
}
