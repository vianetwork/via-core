use async_trait::async_trait;
use bitcoin::{
    key::UntweakedPublicKey,
    secp256k1::{
        ecdsa::Signature as ECDSASignature, schnorr::Signature as SchnorrSignature, All, Keypair,
        Message, PublicKey, Secp256k1, SecretKey,
    },
    Address, CompressedPublicKey, Network, PrivateKey, ScriptBuf,
};

use crate::{
    traits::BitcoinSigner,
    types::{BitcoinError, BitcoinSignerResult},
};

/// KeyManager handles the creation and management of Bitcoin keys and addresses.
/// It provides functionality for signing transactions using both ECDSA and Schnorr signatures.
#[derive(Clone)]
pub struct KeyManager {
    secp: Secp256k1<All>,
    sk: SecretKey,
    address: Address,
    keypair: Keypair,
    internal_key: UntweakedPublicKey,
    script_pubkey: ScriptBuf,
}

impl KeyManager {
    /// Creates a new KeyManager instance from a WIF-encoded private key and network.
    ///
    /// # Arguments
    ///
    /// * `private_key_wif_str` - A WIF-encoded private key string
    /// * `network` - The Bitcoin network (e.g., Mainnet, Testnet)
    ///
    /// # Returns
    ///
    /// A Result containing the KeyManager instance or a BitcoinError
    pub(crate) fn new(private_key_wif_str: &str, network: Network) -> BitcoinSignerResult<Self> {
        let secp = Secp256k1::new();

        let private_key = PrivateKey::from_wif(private_key_wif_str)
            .map_err(|e| BitcoinError::InvalidPrivateKey(e.to_string()))?;

        let sk = private_key.inner;

        let pk = bitcoin::PublicKey::new(sk.public_key(&secp));
        let wpkh = pk.wpubkey_hash().map_err(|_e| {
            BitcoinError::UncompressedPublicKeyError("key is compressed".to_string())
        })?;

        let compressed_pk = CompressedPublicKey::from_private_key(&secp, &private_key)
            .map_err(|e| BitcoinError::CompressedPublicKeyError(e.to_string()))?;

        let address = Address::p2wpkh(&compressed_pk, network);

        let keypair = Keypair::from_secret_key(&secp, &sk);

        let internal_key = keypair.x_only_public_key().0;

        let script_pubkey = ScriptBuf::new_p2wpkh(&wpkh);

        Ok(Self {
            secp,
            sk,
            address,
            keypair,
            internal_key,
            script_pubkey,
        })
    }
}

impl Default for KeyManager {
    fn default() -> Self {
        let secp = Secp256k1::new();
        let sk = PrivateKey::generate(Network::Regtest);
        let keypair = Keypair::from_secret_key(&secp, &sk.inner);
        let compressed_pk = CompressedPublicKey::from_private_key(&secp, &sk)
            .expect("Failed to generate compressed public key");
        let address = Address::p2wpkh(&compressed_pk, Network::Testnet);
        let internal_key = keypair.x_only_public_key().0;
        let script_pubkey = address.script_pubkey();

        Self {
            secp,
            sk: sk.inner,
            address,
            keypair,
            internal_key,
            script_pubkey,
        }
    }
}

#[async_trait]
impl BitcoinSigner for KeyManager {
    fn get_p2wpkh_address(&self) -> BitcoinSignerResult<Address> {
        Ok(self.address.clone())
    }

    fn get_p2wpkh_script_pubkey(&self) -> &ScriptBuf {
        &self.script_pubkey
    }

    fn get_secp_ref(&self) -> &Secp256k1<All> {
        &self.secp
    }

    fn get_internal_key(&self) -> BitcoinSignerResult<UntweakedPublicKey> {
        Ok(self.internal_key)
    }

    fn sign_ecdsa(&self, msg: Message) -> BitcoinSignerResult<ECDSASignature> {
        let signature = self.secp.sign_ecdsa(&msg, &self.sk);
        Ok(signature)
    }

    fn sign_schnorr(&self, msg: Message) -> BitcoinSignerResult<SchnorrSignature> {
        let signature = self.secp.sign_schnorr_no_aux_rand(&msg, &self.keypair);
        Ok(signature)
    }

    fn get_public_key(&self) -> PublicKey {
        self.sk.public_key(&self.secp)
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::{AddressType, Network};

    use super::*;

    #[test]
    fn test_key_manager_default() {
        let key_manager = KeyManager::default();
        assert_eq!(
            key_manager.address.address_type().unwrap(),
            AddressType::P2wpkh
        );
    }

    #[test]
    fn test_key_manager_new() {
        let private_key = "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc";
        let key_manager = KeyManager::new(private_key, Network::Testnet).unwrap();
        assert_eq!(
            key_manager.address.address_type().unwrap(),
            AddressType::P2wpkh
        );
    }

    #[test]
    fn test_sign_ecdsa() {
        let key_manager = KeyManager::default();
        let message = Message::from_digest_slice(&[1; 32]).unwrap();
        let signature = key_manager.sign_ecdsa(message).unwrap();
        assert!(key_manager
            .secp
            .verify_ecdsa(&message, &signature, &key_manager.get_public_key())
            .is_ok());
    }

    #[test]
    fn test_sign_schnorr() {
        let key_manager = KeyManager::default();
        let message = Message::from_digest_slice(&[1; 32]).unwrap();
        let signature = key_manager.sign_schnorr(message).unwrap();
        assert!(key_manager
            .secp
            .verify_schnorr(
                &signature,
                &message,
                &key_manager.get_internal_key().unwrap()
            )
            .is_ok());
    }
}
