use std::{clone::Clone, str::FromStr};

use anyhow::Context;
use secp256k1_musig2::{PublicKey, Secp256k1, SecretKey};
use via_musig2::Signer;

pub fn get_signer(
    private_key: &str,
    verifiers_pub_keys_str: Vec<String>,
) -> anyhow::Result<(Signer)> {
    let secret_key =
        SecretKey::from_str(private_key).context("Error to compute the coordinator sk")?;
    let secp = Secp256k1::new();
    let public_key = PublicKey::from_secret_key(&secp, &secret_key);

    let mut all_pubkeys = Vec::new();
    all_pubkeys.push(public_key);

    let mut signer_index = 0;
    for i in 0..verifiers_pub_keys_str.len() {
        let pk = PublicKey::from_slice(verifiers_pub_keys_str[i].as_bytes())?;
        all_pubkeys.push(pk);
        if pk == public_key {
            signer_index = i;
        }
    }

    let signer = Signer::new(secret_key, signer_index, all_pubkeys.clone())?;
    Ok(signer)
}
