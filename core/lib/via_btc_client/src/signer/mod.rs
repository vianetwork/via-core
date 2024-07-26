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
        unsigned_tx: &mut Transaction,
        input_index: usize,
    ) -> BitcoinSignerResult<()> {
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

        let witness = Witness::p2wpkh(&sig, &self.private_key.inner.public_key(&secp));
        unsigned_tx.input[input_index].witness = witness;

        Ok(())
    }

    async fn sign_reveal(
        &self,
        unsigned_tx: &mut Transaction,
        input_index: usize,
        tapscript: &ScriptBuf,
        leaf_version: LeafVersion,
        control_block: &ControlBlock,
    ) -> BitcoinSignerResult<()> {
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

        unsigned_tx.input[input_index].witness = reveal_witness;

        Ok(())
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
