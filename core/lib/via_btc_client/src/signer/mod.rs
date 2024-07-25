use async_trait::async_trait;
use bitcoin::{
    PrivateKey, Transaction, consensus::{deserialize, serialize},
    psbt::Psbt, EcdsaSighashType, sighash::SighashCache,
    secp256k1::{Secp256k1 as BitcoinSecp256k1, Message},
};
use secp256k1::SecretKey;
use crate::traits::{BitcoinRpc, BitcoinSigner};
use crate::types::{BitcoinError, BitcoinSignerResult};

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
        let transaction: Transaction = deserialize(&hex::decode(unsigned_transaction)
            .map_err(|e| BitcoinError::InvalidTransaction(e.to_string()))?)
            .map_err(|e| BitcoinError::InvalidTransaction(e.to_string()))?;
        let mut psbt = Psbt::from_unsigned_tx(transaction.clone())
            .map_err(|e| BitcoinError::TransactionBuildingError(e.to_string()))?;

        let mut sighash_cache = SighashCache::new(&transaction);
        let secp = BitcoinSecp256k1::new();

        let mut prevouts = Vec::new();
        for input in &transaction.input {
            let prev_tx = self.rpc_client.get_transaction(&input.previous_output.txid).await?;
            let prev_output = prev_tx.output.get(input.previous_output.vout as usize)
                .ok_or_else(|| BitcoinError::InvalidOutpoint(input.previous_output.to_string()))?;
            prevouts.push(prev_output.clone());
        }

        for (input_index, _) in transaction.input.iter().enumerate() {
            let prev_output = prevouts.get(input_index)
                .ok_or_else(|| BitcoinError::InvalidOutpoint(format!("Invalid input index: {}", input_index)))?;
            let script_pubkey = &prev_output.script_pubkey;

            let sighash = sighash_cache.p2wpkh_signature_hash(
                input_index,
                script_pubkey,
                prev_output.value,
                EcdsaSighashType::All,
            ).map_err(|e| BitcoinError::SigningError(e.to_string()))?;

            let message = Message::from_digest_slice(sighash.as_ref())
                .map_err(|e| BitcoinError::SigningError(e.to_string()))?;

            let secret_key = SecretKey::from_slice(&self.private_key.inner[..])
                .map_err(|e| BitcoinError::SigningError(e.to_string()))?;
            let signature = secp.sign_ecdsa(&message, &secret_key);

            let bitcoin_signature = bitcoin::ecdsa::Signature {
                signature,
                sighash_type: EcdsaSighashType::All,
            };

            psbt.inputs[input_index].partial_sigs.insert(
                self.private_key.public_key(&secp),
                bitcoin_signature,
            );
        }

        let signed_tx = psbt.extract_tx()
            .map_err(|e| BitcoinError::TransactionBuildingError(e.to_string()))?;

        Ok(hex::encode(serialize(&signed_tx)))
    }
}