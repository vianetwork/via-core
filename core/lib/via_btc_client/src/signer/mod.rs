use async_trait::async_trait;
use bitcoin::{
    consensus::{deserialize, serialize},
    hashes::Hash,
    psbt::Psbt,
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

    async fn sign_transfer(&self, unsigned_transaction: &str) -> BitcoinSignerResult<String> {
        let (transaction, mut sighash_cache, secp, secret_key) =
            self.init_signing(unsigned_transaction)?;
        let prevouts = self.get_prevouts(&transaction).await?;
        let input_len = transaction.input.len();
        let mut psbt = Psbt::from_unsigned_tx(transaction)
            .map_err(|e| BitcoinError::TransactionBuildingError(e.to_string()))?;

        for (input_index, prev_output) in prevouts.iter().enumerate().take(input_len) {
            let signature = self.sign_ecdsa(
                &mut sighash_cache,
                input_index,
                prev_output,
                &secret_key,
                &secp,
            )?;
            psbt.inputs[input_index]
                .partial_sigs
                .insert(self.private_key.public_key(&secp), signature);
        }

        let signed_tx = psbt
            .extract_tx()
            .map_err(|e| BitcoinError::TransactionBuildingError(e.to_string()))?;

        Ok(hex::encode(serialize(&signed_tx)))
    }

    async fn sign_reveal_transaction(
        &self,
        unsigned_transaction: &str,
        tapscript: &ScriptBuf,
        leaf_version: LeafVersion,
        control_block: &ControlBlock,
    ) -> BitcoinSignerResult<String> {
        let (mut transaction, mut sighash_cache, secp, secret_key) =
            self.init_signing(unsigned_transaction)?;
        let prevouts = self.get_prevouts(&transaction).await?;
        let keypair = Keypair::from_secret_key(&secp, &secret_key);

        // fee part

        let fee_signature =
            self.sign_ecdsa(&mut sighash_cache, 0, &prevouts[0], &secret_key, &secp)?;
        let fee_witness =
            Witness::p2wpkh(&fee_signature, &self.private_key.inner.public_key(&secp));

        // reveal part

        let reveal_sighash = sighash_cache
            .taproot_script_spend_signature_hash(
                1,
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

        transaction.input[0].witness = fee_witness;
        transaction.input[1].witness = reveal_witness;

        Ok(hex::encode(serialize(&transaction)))
    }
}

impl BasicSigner<'_> {
    fn init_signing(
        &self,
        unsigned_transaction: &str,
    ) -> BitcoinSignerResult<(
        Transaction,
        SighashCache<Transaction>,
        Secp256k1<secp256k1::All>,
        SecretKey,
    )> {
        let transaction: Transaction = deserialize(
            &hex::decode(unsigned_transaction)
                .map_err(|e| BitcoinError::InvalidTransaction(e.to_string()))?,
        )
        .map_err(|e| BitcoinError::InvalidTransaction(e.to_string()))?;
        let sighash_cache = SighashCache::new(transaction.clone());
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(&self.private_key.inner[..])
            .map_err(|e| BitcoinError::SigningError(e.to_string()))?;

        Ok((transaction, sighash_cache, secp, secret_key))
    }

    fn sign_ecdsa(
        &self,
        sighash_cache: &mut SighashCache<Transaction>,
        input_index: usize,
        prev_output: &TxOut,
        secret_key: &SecretKey,
        secp: &Secp256k1<secp256k1::All>,
    ) -> BitcoinSignerResult<bitcoin::ecdsa::Signature> {
        let sighash = sighash_cache
            .p2wpkh_signature_hash(
                input_index,
                &prev_output.script_pubkey,
                prev_output.value,
                EcdsaSighashType::All,
            )
            .map_err(|e| BitcoinError::SigningError(e.to_string()))?;

        let message = Message::from_digest_slice(sighash.as_ref())
            .map_err(|e| BitcoinError::SigningError(e.to_string()))?;

        let signature = secp.sign_ecdsa(&message, secret_key);

        Ok(bitcoin::ecdsa::Signature {
            signature,
            sighash_type: EcdsaSighashType::All,
        })
    }

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
