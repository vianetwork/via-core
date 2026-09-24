use base64::Engine;
use bitcoin::secp256k1::{
    rand::{rngs::OsRng, RngCore},
    Message, PublicKey, Secp256k1, SecretKey,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_BODY: usize = 8 * 1024 * 1024;

/// Bind the operation and audience as well as the payload, following AWS SigV4's
/// canonical-request principle; this envelope uses Bitcoin keys, not AWS HMAC credentials.
/// https://github.com/boto/botocore/blob/a3bbf61a0a3548c6bc7b68dd0a23bb6242a8e630/botocore/auth.py
/// RFC 9421 covered components and RFC 9530 authenticated digests inform the boundary,
/// but this exact-byte envelope implements neither RFC's HTTP field format:
/// https://www.rfc-editor.org/rfc/rfc9421.html#section-2
/// https://www.rfc-editor.org/rfc/rfc9530.html#section-6.3
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binding {
    pub version: u8,
    pub sequencer_version: String,
    pub principal: String,
    pub audience: String,
    pub method: String,
    pub target: String,
    pub round: [u8; 32],
    pub content: [u8; 32],
    pub challenge: [u8; 32],
    pub timestamp: i64,
    pub status: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub binding: Binding,
    pub body: Vec<u8>,
    pub signature: String,
}

pub fn random_id() -> [u8; 32] {
    let mut value = [0; 32];
    OsRng.fill_bytes(&mut value);
    value
}

pub fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

impl Envelope {
    fn message(binding: &Binding, body: &[u8]) -> anyhow::Result<Message> {
        // Domain separated, length-delimited metadata and exact application bytes.
        let bytes = bincode::serialize(&("via-withdrawal-http-v1", binding, body))?;
        Ok(Message::from_digest(digest(&bytes)))
    }

    /// Key use is separate from business authorization, as in zkSync Era's OperatorSigner.
    /// Producing a signature does not establish verifier membership or withdrawal eligibility:
    /// https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/operator_signer/src/lib.rs
    pub fn sign(binding: Binding, body: Vec<u8>, key: &SecretKey) -> anyhow::Result<Self> {
        let signature = Secp256k1::new().sign_ecdsa(&Self::message(&binding, &body)?, key);
        Ok(Self {
            binding,
            body,
            signature: base64::engine::general_purpose::STANDARD
                .encode(signature.serialize_compact()),
        })
    }

    pub fn verify(&self, key: &PublicKey, now: i64, max_age: u8) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.binding.version == 1,
            "Unsupported authentication version"
        );
        let age = now
            .checked_sub(self.binding.timestamp)
            .ok_or_else(|| anyhow::anyhow!("Invalid timestamp"))?;
        anyhow::ensure!(
            age >= -30 && age <= i64::from(max_age),
            "Expired authentication"
        );
        anyhow::ensure!(
            self.binding.principal == key.to_string(),
            "Wrong signing principal"
        );
        let signature = base64::engine::general_purpose::STANDARD.decode(&self.signature)?;
        let signature = bitcoin::secp256k1::ecdsa::Signature::from_compact(&signature)?;
        Secp256k1::new().verify_ecdsa(
            &Self::message(&self.binding, &self.body)?,
            &signature,
            key,
        )?;
        Ok(())
    }

    pub fn verify_response(
        &self,
        request: &Binding,
        key: &PublicKey,
        status: u16,
        now: i64,
        max_age: u8,
    ) -> anyhow::Result<()> {
        self.verify(key, now, max_age)?;
        let mut expected = request.clone();
        expected.principal = key.to_string();
        expected.audience = request.principal.clone();
        expected.status = status;
        expected.timestamp = self.binding.timestamp;
        anyhow::ensure!(
            self.binding == expected,
            "Response request binding mismatch"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (SecretKey, PublicKey, Binding) {
        let key = SecretKey::from_slice(&[7; 32]).unwrap();
        let public = PublicKey::from_secret_key(&Secp256k1::new(), &key);
        let binding = Binding {
            version: 1,
            sequencer_version: "0.1.0".into(),
            principal: public.to_string(),
            audience: "coordinator".into(),
            method: "POST".into(),
            target: "/session/nonce".into(),
            round: [1; 32],
            content: [2; 32],
            challenge: [3; 32],
            timestamp: 100,
            status: 0,
        };
        (key, public, binding)
    }
    #[test]
    fn exact_body_and_operation_cannot_be_substituted() {
        let (key, public, binding) = fixture();
        let mut envelope = Envelope::sign(binding, b"{\"nonce\":1}".to_vec(), &key).unwrap();
        envelope.verify(&public, 100, 30).unwrap();
        envelope.body.push(b' ');
        assert!(envelope.verify(&public, 100, 30).is_err());
        envelope.body.pop();
        envelope.binding.target = "/session/signature".into();
        assert!(envelope.verify(&public, 100, 30).is_err());
    }
    #[test]
    fn response_cannot_cross_challenges_rounds_or_audiences() {
        let (key, public, request) = fixture();
        let mut reply = request.clone();
        reply.audience = request.principal.clone();
        reply.status = 200;
        let envelope = Envelope::sign(reply, b"[]".to_vec(), &key).unwrap();
        envelope
            .verify_response(&request, &public, 200, 100, 30)
            .unwrap();
        for changed in [0, 1, 2] {
            let mut other = request.clone();
            match changed {
                0 => other.challenge[0] ^= 1,
                1 => other.round[0] ^= 1,
                _ => other.principal.push('x'),
            }
            assert!(envelope
                .verify_response(&other, &public, 200, 100, 30)
                .is_err());
        }
        assert!(envelope
            .verify_response(&request, &public, 200, 131, 30)
            .is_err());
    }
}
