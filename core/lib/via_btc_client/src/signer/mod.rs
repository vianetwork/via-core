use async_trait::async_trait;
use bitcoin::{
    hashes::Hash,
    secp256k1::{Message, Secp256k1},
    sighash::{Prevouts, SighashCache},
    taproot::{ControlBlock, LeafVersion},
    EcdsaSighashType, PrivateKey, ScriptBuf, TapLeafHash, TapSighashType, Transaction, TxOut,
    Witness,
};
use secp256k1::{Keypair, SecretKey};

use crate::{
    traits::{BitcoinRpc, BitcoinSigner},
    types::{BitcoinError, BitcoinSignerResult},
};

pub struct BasicSigner<'a> {
    private_key: PrivateKey,
    rpc_client: &'a dyn BitcoinRpc,
}

#[async_trait]
impl<'a> BitcoinSigner<'a> for BasicSigner<'a> {
    fn new(private_key: &str, rpc_client: &'a dyn BitcoinRpc) -> BitcoinSignerResult<Self> {
        let private_key = PrivateKey::from_wif(private_key)
            .map_err(|e| BitcoinError::InvalidPrivateKey(e.to_string()))?;

        Ok(Self {
            private_key,
            rpc_client,
        })
    }

    async fn sign_ecdsa(
        &self,
        unsigned_tx: &Transaction,
        input_index: usize,
    ) -> BitcoinSignerResult<Witness> {
        let prevouts = self.get_prevouts(unsigned_tx).await?;
        let mut sighash_cache = SighashCache::new(unsigned_tx.clone());
        let secp = Secp256k1::new();

        let secret_key = SecretKey::from_slice(&self.private_key.inner[..])
            .map_err(|e| BitcoinError::SigningError(e.to_string()))?;

        let sighash = sighash_cache
            .p2wpkh_signature_hash(
                input_index,
                &prevouts[input_index].script_pubkey,
                prevouts[input_index].value,
                EcdsaSighashType::All,
            )
            .map_err(|e| BitcoinError::SigningError(e.to_string()))?;

        let message = Message::from_digest_slice(sighash.as_ref())
            .map_err(|e| BitcoinError::SigningError(e.to_string()))?;

        let signature = secp.sign_ecdsa(&message, &secret_key);

        let sig = bitcoin::ecdsa::Signature {
            signature,
            sighash_type: EcdsaSighashType::All,
        };

        Ok(Witness::p2wpkh(
            &sig,
            &self.private_key.inner.public_key(&secp),
        ))
    }

    async fn sign_reveal(
        &self,
        unsigned_tx: &Transaction,
        input_index: usize,
        tapscript: &ScriptBuf,
        leaf_version: LeafVersion,
        control_block: &ControlBlock,
    ) -> BitcoinSignerResult<Witness> {
        let prevouts = self.get_prevouts(unsigned_tx).await?;
        let mut sighash_cache = SighashCache::new(unsigned_tx.clone());
        let secp = Secp256k1::new();

        let secret_key = SecretKey::from_slice(&self.private_key.inner[..])
            .map_err(|e| BitcoinError::SigningError(e.to_string()))?;
        let keypair = Keypair::from_secret_key(&secp, &secret_key);

        let reveal_sighash = sighash_cache
            .taproot_script_spend_signature_hash(
                input_index,
                &Prevouts::All(&prevouts),
                TapLeafHash::from_script(tapscript, leaf_version),
                TapSighashType::All,
            )
            .map_err(|e| BitcoinError::SigningError(e.to_string()))?;

        let reveal_msg = Message::from_digest(reveal_sighash.to_byte_array());
        let reveal_signature = secp.sign_schnorr_no_aux_rand(&reveal_msg, &keypair);

        let tap_sig = bitcoin::taproot::Signature {
            signature: reveal_signature,
            sighash_type: TapSighashType::All,
        };

        let mut reveal_witness = Witness::new();
        reveal_witness.push(tap_sig.serialize());
        reveal_witness.push(tapscript.as_bytes());
        reveal_witness.push(control_block.serialize());

        Ok(reveal_witness)
    }
}

impl<'a> BasicSigner<'a> {
    async fn get_prevouts(&self, tx: &Transaction) -> BitcoinSignerResult<Vec<TxOut>> {
        let mut prevouts = Vec::with_capacity(tx.input.len());
        for input in &tx.input {
            let prev_tx = self
                .rpc_client
                .get_transaction(&input.previous_output.txid)
                .await?;
            let prev_output = prev_tx
                .output
                .get(input.previous_output.vout as usize)
                .ok_or_else(|| BitcoinError::InvalidOutpoint(input.previous_output.to_string()))?;
            prevouts.push(prev_output.clone());
        }
        Ok(prevouts)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::{
        absolute::LockTime, key::UntweakedPublicKey, taproot::TaprootMerkleBranch,
        transaction::Version, Address, Amount, Block, OutPoint, Sequence, Txid,
    };
    use bitcoincore_rpc::json::{EstimateMode, EstimateSmartFeeResult, GetRawTransactionResult};
    use mockall::{mock, predicate::*};
    use secp256k1::Parity;
    use types::BitcoinRpcResult;

    use super::*;
    use crate::types;

    mock! {
        BitcoinRpc {}
        #[async_trait]
        impl BitcoinRpc for BitcoinRpc {
            async fn get_balance(&self, address: &Address) -> BitcoinRpcResult<u64>;
            async fn send_raw_transaction(&self, tx_hex: &str) -> BitcoinRpcResult<Txid>;
            async fn list_unspent(&self, address: &Address) -> BitcoinRpcResult<Vec<OutPoint>>;
            async fn get_transaction(&self, tx_id: &Txid) -> BitcoinRpcResult<Transaction>;
            async fn get_block_count(&self) -> BitcoinRpcResult<u64>;
            async fn get_block(&self, block_height: u128) -> BitcoinRpcResult<Block>;
            async fn get_best_block_hash(&self) -> BitcoinRpcResult<bitcoin::BlockHash>;
            async fn get_raw_transaction_info(
                &self,
                txid: &Txid,
            ) -> BitcoinRpcResult<GetRawTransactionResult>;
            async fn estimate_smart_fee(
                &self,
                conf_target: u16,
                estimate_mode: Option<EstimateMode>,
            ) -> BitcoinRpcResult<EstimateSmartFeeResult>;
        }
    }

    #[tokio::test]
    async fn test_new_signer() {
        let mock_rpc = MockBitcoinRpc::new();
        let private_key = "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc";

        let signer = BasicSigner::new(private_key, &mock_rpc);
        assert!(signer.is_ok());
    }

    #[tokio::test]
    async fn test_sign_ecdsa() {
        let mut mock_rpc = MockBitcoinRpc::new();
        let private_key =
            PrivateKey::from_wif("cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc").unwrap();
        let secp = Secp256k1::new();
        let public_key = private_key.public_key(&secp);
        let pubkey_hash = public_key.wpubkey_hash().unwrap();

        let prev_tx = Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(100000),
                script_pubkey: ScriptBuf::new_p2wpkh(&pubkey_hash),
            }],
        };

        mock_rpc
            .expect_get_transaction()
            .returning(move |_| Ok(prev_tx.clone()));

        let signer = BasicSigner::new(&private_key.to_wif(), &mock_rpc).unwrap();

        let unsigned_tx = Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: OutPoint::new(Txid::all_zeros(), 0),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::from_hex("0xffffffff").unwrap(),
                witness: Witness::new(),
            }],
            output: vec![],
        };

        let result = signer.sign_ecdsa(&unsigned_tx, 0).await;

        assert!(result.is_ok());
        assert!(!result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_sign_reveal() {
        let mut mock_rpc = MockBitcoinRpc::new();
        let private_key =
            PrivateKey::from_wif("cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc").unwrap();
        let secp = Secp256k1::new();
        let public_key = private_key.public_key(&secp);

        let internal_key_bytes = [2u8; 32];
        let internal_key = UntweakedPublicKey::from_slice(&internal_key_bytes).unwrap();

        let control_block = ControlBlock {
            leaf_version: LeafVersion::TapScript,
            output_key_parity: Parity::Even,
            internal_key,
            merkle_branch: TaprootMerkleBranch::default(),
        };

        let prev_tx = Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(100000),
                script_pubkey: ScriptBuf::new_p2tr(
                    &secp,
                    UntweakedPublicKey::from(public_key.inner),
                    None,
                ),
            }],
        };

        mock_rpc
            .expect_get_transaction()
            .returning(move |_| Ok(prev_tx.clone()));

        let signer = BasicSigner::new(&private_key.to_wif(), &mock_rpc).unwrap();

        let unsigned_tx = Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: OutPoint::new(Txid::all_zeros(), 0),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![],
        };

        let tapscript = ScriptBuf::new();
        let leaf_version = LeafVersion::TapScript;

        let result = signer
            .sign_reveal(&unsigned_tx, 0, &tapscript, leaf_version, &control_block)
            .await;

        assert!(result.is_ok());
        assert!(!result.unwrap().is_empty());
    }
    #[tokio::test]
    async fn test_get_prevouts() {
        let mut mock_rpc = MockBitcoinRpc::new();
        let private_key = "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc";

        let prev_tx1 = Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(100000),
                script_pubkey: ScriptBuf::new(),
            }],
        };
        let prev_tx2 = Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(200000),
                script_pubkey: ScriptBuf::new(),
            }],
        };

        mock_rpc
            .expect_get_transaction()
            .times(2)
            .returning(move |txid| {
                if txid == &Txid::all_zeros() {
                    Ok(prev_tx1.clone())
                } else {
                    Ok(prev_tx2.clone())
                }
            });

        let signer = BasicSigner::new(private_key, &mock_rpc).unwrap();

        let tx = Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: vec![
                bitcoin::TxIn {
                    previous_output: OutPoint::new(Txid::all_zeros(), 0),
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::from_hex("0xffffffff").unwrap(),
                    witness: Witness::new(),
                },
                bitcoin::TxIn {
                    previous_output: OutPoint::new(
                        Txid::from_str(
                            "1111111111111111111111111111111111111111111111111111111111111111",
                        )
                        .unwrap(),
                        0,
                    ),
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::from_hex("0xffffffff").unwrap(),
                    witness: Witness::new(),
                },
            ],
            output: vec![],
        };

        let result = signer.get_prevouts(&tx).await;
        assert!(result.is_ok());
        let prevouts = result.unwrap();
        assert_eq!(prevouts.len(), 2);
        assert_eq!(prevouts[0].value.to_sat(), 100000);
        assert_eq!(prevouts[1].value.to_sat(), 200000);
    }
}
