use async_trait::async_trait;
use bitcoin::{
    key::UntweakedPublicKey,
    secp256k1::{All, Keypair, Message, Secp256k1, SecretKey},
    Address, CompressedPublicKey, Network, PrivateKey, ScriptBuf,
};
use secp256k1::{
    ecdsa::Signature as ECDSASignature, schnorr::Signature as SchnorrSignature, PublicKey,
};

use crate::{
    traits::BitcoinSigner,
    types::{BitcoinError, BitcoinSignerResult},
};

pub struct KeyManager {
    pub secp: Secp256k1<All>,
    sk: SecretKey,
    address: Address,
    keypair: Keypair,
    internal_key: UntweakedPublicKey,
    script_pubkey: ScriptBuf,
}

#[async_trait]
impl BitcoinSigner for KeyManager {
    fn new(private_key_wif_str: &str, network: Network) -> BitcoinSignerResult<Self> {
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

        let res = KeyManager {
            secp,
            sk,
            address,
            keypair,
            internal_key,
            script_pubkey,
        };

        Ok(res)
    }

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

// #[cfg(test)]
// mod tests {
//     use std::str::FromStr;

//     use bitcoin::{
//         absolute::LockTime, key::UntweakedPublicKey, taproot::TaprootMerkleBranch,
//         transaction::Version, Address, Amount, Block, OutPoint, Sequence, Txid,
//     };
//     use bitcoincore_rpc::json::{EstimateMode, EstimateSmartFeeResult, GetRawTransactionResult};
//     use mockall::{mock, predicate::*};
//     use secp256k1::Parity;
//     use types::BitcoinRpcResult;

//     use super::*;
//     use crate::types;

//     #[tokio::test]
//     async fn test_new_signer() {
//         let private_key = "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc";

//         let signer = BasicSigner::new(private_key);
//         assert!(signer.is_ok());
//     }

//     #[tokio::test]
//     async fn test_sign_ecdsa() {
//         let private_key =
//             PrivateKey::from_wif("cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc").unwrap();
//         let secp = Secp256k1::new();
//         let public_key = private_key.public_key(&secp);
//         let pubkey_hash = public_key.wpubkey_hash().unwrap();

//         let prev_tx = Transaction {
//             version: Version(2),
//             lock_time: LockTime::ZERO,
//             input: vec![],
//             output: vec![TxOut {
//                 value: Amount::from_sat(100000),
//                 script_pubkey: ScriptBuf::new_p2wpkh(&pubkey_hash),
//             }],
//         };

//         let signer = BasicSigner::new(&private_key.to_wif()).unwrap();

//         let unsigned_tx = Transaction {
//             version: Version(2),
//             lock_time: LockTime::ZERO,
//             input: vec![bitcoin::TxIn {
//                 previous_output: OutPoint::new(Txid::all_zeros(), 0),
//                 script_sig: ScriptBuf::new(),
//                 sequence: Sequence::from_hex("0xffffffff").unwrap(),
//                 witness: Witness::new(),
//             }],
//             output: vec![],
//         };

//         let result = signer.sign_ecdsa(&unsigned_tx, 0).await;

//         assert!(result.is_ok());
//         assert!(!result.unwrap().is_empty());
//     }

//     #[tokio::test]
//     async fn test_sign_reveal() {
//         let private_key =
//             PrivateKey::from_wif("cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc").unwrap();
//         let secp = Secp256k1::new();
//         let public_key = private_key.public_key(&secp);

//         let internal_key_bytes = [2u8; 32];
//         let internal_key = UntweakedPublicKey::from_slice(&internal_key_bytes).unwrap();

//         let control_block = ControlBlock {
//             leaf_version: LeafVersion::TapScript,
//             output_key_parity: Parity::Even,
//             internal_key,
//             merkle_branch: TaprootMerkleBranch::default(),
//         };

//         let prev_tx = Transaction {
//             version: Version(2),
//             lock_time: LockTime::ZERO,
//             input: vec![],
//             output: vec![TxOut {
//                 value: Amount::from_sat(100000),
//                 script_pubkey: ScriptBuf::new_p2tr(
//                     &secp,
//                     UntweakedPublicKey::from(public_key.inner),
//                     None,
//                 ),
//             }],
//         };

//         let signer = BasicSigner::new(&private_key.to_wif()).unwrap();

//         let unsigned_tx = Transaction {
//             version: Version(2),
//             lock_time: LockTime::ZERO,
//             input: vec![bitcoin::TxIn {
//                 previous_output: OutPoint::new(Txid::all_zeros(), 0),
//                 script_sig: ScriptBuf::new(),
//                 sequence: Sequence::MAX,
//                 witness: Witness::new(),
//             }],
//             output: vec![],
//         };

//         let tapscript = ScriptBuf::new();
//         let leaf_version = LeafVersion::TapScript;

//         let result = signer
//             .sign_schnorr(&unsigned_tx, 0, &tapscript, leaf_version, &control_block)
//             .await;

//         assert!(result.is_ok());
//         assert!(!result.unwrap().is_empty());
//     }
// }
