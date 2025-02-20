use anyhow::Context;
use base64::Engine;
use bitcoin::secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Signs a request payload using the verifier's private key.
pub fn sign_request<T: Serialize>(payload: &T, secret_key: &SecretKey) -> anyhow::Result<String> {
    let secp = Secp256k1::new();

    // Serialize and hash the payload.
    let payload_bytes = serde_json::to_vec(payload).context("Failed to serialize payload")?;
    let hash = Sha256::digest(&payload_bytes);
    let message = Message::from_digest_slice(hash.as_ref()).context("Hash is not 32 bytes")?;

    // Sign the message.
    let sig = secp.sign_ecdsa(&message, secret_key);
    // Encode the compact 64-byte signature in base64.
    let sig_bytes = sig.serialize_compact();
    Ok(base64::engine::general_purpose::STANDARD.encode(sig_bytes))
}

/// Verifies a request signature using the verifier's public key.
pub fn verify_signature<T: Serialize>(
    payload: &T,
    signature_b64: &str,
    public_key: &PublicKey,
) -> anyhow::Result<bool> {
    let secp = Secp256k1::new();

    // Decode the base64 signature.
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_b64)
        .context("Failed to decode base64 signature")?;
    let sig = bitcoin::secp256k1::ecdsa::Signature::from_compact(&sig_bytes)
        .context("Failed to parse signature from compact form")?;

    // Serialize and hash the payload.
    let payload_bytes = serde_json::to_vec(payload).context("Failed to serialize payload")?;
    let hash = Sha256::digest(&payload_bytes);
    let message = Message::from_digest_slice(hash.as_ref()).context("Hash is not 32 bytes")?;

    // Verify the signature.
    Ok(secp.verify_ecdsa(&message, &sig, public_key).is_ok())
}

#[cfg(test)]
mod tests {
    use bitcoin::secp256k1::rand::rngs::OsRng;
    use serde_json::json;

    use super::*;

    #[test]
    fn test_signature_verification() {
        let secp = Secp256k1::new();
        let (secret_key, public_key) = secp.generate_keypair(&mut OsRng);

        let payload = json!({
            "test": "data",
            "number": 123
        });

        // Sign the payload.
        let signature = sign_request(&payload, &secret_key).expect("Signature generation failed");

        // Verify the signature.
        assert!(verify_signature(&payload, &signature, &public_key)
            .expect("Signature verification failed"));

        // Verify that a wrong public key does not verify.
        let (_, wrong_public_key) = secp.generate_keypair(&mut OsRng);
        assert!(!verify_signature(&payload, &signature, &wrong_public_key)
            .expect("Verification with wrong key unexpectedly succeeded"));
    }
}
