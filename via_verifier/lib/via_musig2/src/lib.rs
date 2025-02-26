use std::fmt;

use bitcoin::TapTweakHash;
use musig2::{
    verify_single, CompactSignature, FirstRound, KeyAggContext, PartialSignature, PubNonce,
    SecNonceSpices, SecondRound,
};
use secp256k1_musig2::{PublicKey, Secp256k1, SecretKey};
pub mod transaction_builder;
pub mod utxo_manager;

#[derive(Debug)]
pub enum MusigError {
    Musig2Error(String),
    InvalidSignerIndex,
    MissingNonces,
    MissingPartialSignatures,
    InvalidState(String),
}

impl fmt::Display for MusigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MusigError::Musig2Error(e) => write!(f, "MuSig2 error: {}", e),
            MusigError::InvalidSignerIndex => write!(f, "Invalid signer index"),
            MusigError::MissingNonces => write!(f, "Missing required nonces"),
            MusigError::MissingPartialSignatures => {
                write!(f, "Missing required partial signatures")
            }
            MusigError::InvalidState(s) => write!(f, "Invalid state: {}", s),
        }
    }
}

impl std::error::Error for MusigError {}

/// Represents a single signer in the MuSig2 protocol
pub struct Signer {
    secret_key: SecretKey,
    public_key: PublicKey,
    signer_index: usize,
    key_agg_ctx: KeyAggContext,
    first_round: Option<FirstRound>,
    second_round: Option<SecondRound<Vec<u8>>>,
    message: Vec<u8>,
    nonce_submitted: bool,
    partial_sig_submitted: bool,
}

impl fmt::Debug for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signer")
            .field("public_key", &self.public_key)
            .field("signer_index", &self.signer_index)
            .field("key_agg_ctx", &self.key_agg_ctx)
            .field("message", &self.message)
            .field("nonce_submitted", &self.nonce_submitted)
            .field("partial_sig_submitted", &self.partial_sig_submitted)
            .finish()
    }
}

impl Signer {
    /// Create a new signer with the given secret key and index
    pub fn new(
        secret_key: SecretKey,
        signer_index: usize,
        all_pubkeys: Vec<PublicKey>,
    ) -> Result<Self, MusigError> {
        let secp = Secp256k1::new();
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);

        // Verify that signer_index is valid and matches the public key
        if signer_index >= all_pubkeys.len() {
            return Err(MusigError::InvalidSignerIndex);
        }
        if all_pubkeys[signer_index] != public_key {
            return Err(MusigError::Musig2Error(
                "Public key at signer_index does not match derived public key".into(),
            ));
        }

        let mut musig_key_agg_cache =
            KeyAggContext::new(all_pubkeys).map_err(|e| MusigError::Musig2Error(e.to_string()))?;

        let agg_pubkey = musig_key_agg_cache.aggregated_pubkey::<secp256k1_musig2::PublicKey>();
        let (xonly_agg_key, _) = agg_pubkey.x_only_public_key();

        // Convert to bitcoin XOnlyPublicKey first
        let internal_key = bitcoin::XOnlyPublicKey::from_slice(&xonly_agg_key.serialize())
            .map_err(|e| {
                MusigError::Musig2Error(format!(
                    "Failed to convert to bitcoin XOnlyPublicKey: {}",
                    e
                ))
            })?;

        // Calculate taproot tweak
        let tap_tweak = TapTweakHash::from_key_and_tweak(internal_key, None);
        let tweak = tap_tweak.to_scalar();
        let tweak_bytes = tweak.to_be_bytes();
        let musig2_compatible_tweak = secp256k1_musig2::Scalar::from_be_bytes(tweak_bytes).unwrap();
        // Apply tweak to the key aggregation context before signing
        musig_key_agg_cache = musig_key_agg_cache
            .with_xonly_tweak(musig2_compatible_tweak)
            .map_err(|e| MusigError::Musig2Error(format!("Failed to apply tweak: {}", e)))?;

        Ok(Self {
            secret_key,
            public_key,
            signer_index,
            key_agg_ctx: musig_key_agg_cache,
            first_round: None,
            second_round: None,
            message: Vec::new(),
            nonce_submitted: false,
            partial_sig_submitted: false,
        })
    }

    /// Get the aggregated public key for all signers
    pub fn aggregated_pubkey(&self) -> PublicKey {
        self.key_agg_ctx.aggregated_pubkey()
    }

    /// Start the signing session with a message
    pub fn start_signing_session(&mut self, message: Vec<u8>) -> Result<PubNonce, MusigError> {
        self.message = message.clone();

        let msg_array = message.as_slice();

        let first_round = FirstRound::new(
            self.key_agg_ctx.clone(),
            rand::random::<[u8; 32]>(),
            self.signer_index,
            SecNonceSpices::new()
                .with_seckey(self.secret_key)
                .with_message(&msg_array),
        )
        .map_err(|e| MusigError::Musig2Error(e.to_string()))?;

        let nonce = first_round.our_public_nonce();
        self.first_round = Some(first_round);
        Ok(nonce)
    }

    /// Receive a nonce from another participant
    pub fn receive_nonce(
        &mut self,
        signer_index: usize,
        nonce: PubNonce,
    ) -> Result<(), MusigError> {
        let first_round = self
            .first_round
            .as_mut()
            .ok_or_else(|| MusigError::InvalidState("First round not initialized".into()))?;

        first_round
            .receive_nonce(signer_index, nonce)
            .map_err(|e| MusigError::Musig2Error(e.to_string()))?;
        Ok(())
    }

    /// Create partial signature
    pub fn create_partial_signature(&mut self) -> Result<PartialSignature, MusigError> {
        let msg_array = self.message.clone();

        let first_round = self
            .first_round
            .take()
            .ok_or_else(|| MusigError::InvalidState("First round not initialized".into()))?;

        let second_round = first_round
            .finalize(self.secret_key, msg_array)
            .map_err(|e| MusigError::Musig2Error(e.to_string()))?;

        let partial_sig = second_round.our_signature();
        self.second_round = Some(second_round);
        Ok(partial_sig)
    }

    /// Receive partial signature from another signer
    pub fn receive_partial_signature(
        &mut self,
        signer_index: usize,
        partial_sig: PartialSignature,
    ) -> Result<(), MusigError> {
        let second_round = self
            .second_round
            .as_mut()
            .ok_or_else(|| MusigError::InvalidState("Second round not initialized".into()))?;

        second_round
            .receive_signature(signer_index, partial_sig)
            .map_err(|e| MusigError::Musig2Error(e.to_string()))?;
        Ok(())
    }

    /// Create final signature
    pub fn create_final_signature(&mut self) -> Result<CompactSignature, MusigError> {
        let second_round = self
            .second_round
            .take()
            .ok_or_else(|| MusigError::InvalidState("Second round not initialized".into()))?;

        second_round
            .finalize()
            .map_err(|e| MusigError::Musig2Error(e.to_string()))
    }

    pub fn signer_index(&self) -> usize {
        self.signer_index
    }

    pub fn has_not_started(&self) -> bool {
        self.first_round.is_none()
    }

    pub fn has_submitted_nonce(&self) -> bool {
        self.nonce_submitted
    }

    pub fn mark_nonce_submitted(&mut self) {
        self.nonce_submitted = true;
    }

    pub fn our_nonce(&self) -> Option<PubNonce> {
        self.first_round
            .as_ref()
            .map(|round| round.our_public_nonce())
    }

    pub fn has_created_partial_sig(&self) -> bool {
        self.second_round.is_some()
    }

    pub fn mark_partial_sig_submitted(&mut self) {
        self.partial_sig_submitted = true;
    }
}

/// Helper function to verify a complete signature
pub fn verify_signature(
    pubkey: PublicKey,
    signature: CompactSignature,
    message: &[u8],
) -> Result<(), MusigError> {
    verify_single(pubkey, signature, message).map_err(|e| MusigError::Musig2Error(e.to_string()))
}

#[cfg(test)]
mod tests {
    use rand::rngs::OsRng;

    use super::*;

    #[test]
    fn test_signer_lifecycle() -> Result<(), MusigError> {
        let mut rng = OsRng;
        let secret_key_1 = SecretKey::new(&mut rng);
        let secret_key_2 = SecretKey::new(&mut rng);

        let secp = Secp256k1::new();
        let public_key_1 = PublicKey::from_secret_key(&secp, &secret_key_1);
        let public_key_2 = PublicKey::from_secret_key(&secp, &secret_key_2);

        let pubkeys = vec![public_key_1, public_key_2];

        let mut signer1 = Signer::new(secret_key_1, 0, pubkeys.clone())?;
        let mut signer2 = Signer::new(secret_key_2, 1, pubkeys)?;

        // Generate and exchange nonces
        let message = b"test message".to_vec();
        let nonce1 = signer1.start_signing_session(message.clone())?;
        let nonce2 = signer2.start_signing_session(message.clone())?;

        signer1.receive_nonce(1, nonce2)?;
        signer2.receive_nonce(0, nonce1)?;

        // Create partial signatures
        let partial_sig1 = signer1.create_partial_signature()?;
        let partial_sig2 = signer2.create_partial_signature()?;

        // Exchange partial signatures
        signer1.receive_partial_signature(1, partial_sig2)?;
        signer2.receive_partial_signature(0, partial_sig1)?;

        // Create final signatures
        let final_sig1 = signer1.create_final_signature()?;
        let final_sig2 = signer2.create_final_signature()?;

        assert_eq!(
            final_sig1.serialize(),
            final_sig2.serialize(),
            "Final signatures should match"
        );
        // Verify the signature
        verify_signature(signer1.aggregated_pubkey(), final_sig1, &message)?;

        Ok(())
    }
}
