use std::{env, sync::Arc};

use anyhow::Result;
use bitcoin::{
    absolute,
    consensus::encode::serialize_hex,
    opcodes::all::OP_RETURN,
    script::{Builder, PushBytesBuf},
    secp256k1::{Message, Secp256k1},
    sighash::{EcdsaSighashType, SighashCache},
    transaction, Address, Amount, CompressedPublicKey, PrivateKey, ScriptBuf, Sequence,
    Transaction, TxIn, TxOut, Witness,
};
use tracing::info;
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps, types::NodeAuth};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let secp = Secp256k1::new();

    let args: Vec<String> = env::args().collect();
    let fees = Amount::from_btc(0.0001)?;

    let pk = args[1].clone();

    let network: bitcoin::Network = args[2].parse().expect("Invalid network value");
    let rpc_url = args[3].clone();
    let rpc_username = args[4].clone();
    let rpc_password = args[5].clone();
    let prefix = args[6].clone();
    let data = args[7].clone();

    // const OP_RETURN_UPDATE_GOVERNANCE_PREFIX: &[u8] = b"VIA_PROTOCOL:GOV";

    let private_key = PrivateKey::from_wif(&pk).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let pk = private_key.inner.public_key(&secp);
    let compressed_pk = CompressedPublicKey::from_private_key(&secp, &private_key)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let address = Address::p2wpkh(&compressed_pk, network);

    let auth = NodeAuth::UserPass(rpc_username.to_string(), rpc_password.to_string());
    let config = ViaBtcClientConfig {
        network: network.to_string(),
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };
    let client = Arc::new(BitcoinClient::new(&rpc_url, auth, config)?);

    // Fetch UTXOs available at our address.
    let all_utxos = client.fetch_utxos(&address).await?;

    // Select only the UTXOs needed to cover the total amount (amount + fees)
    let total_needed = fees;
    let mut selected_utxos = Vec::new();
    let mut input_amount = Amount::from_sat(0);
    for (outpoint, txout) in all_utxos.into_iter() {
        selected_utxos.push((outpoint, txout));
        input_amount += selected_utxos.last().unwrap().1.value;
        if input_amount >= total_needed {
            break;
        }
    }

    if input_amount < total_needed {
        return Err(anyhow::anyhow!("Insufficient funds"));
    }

    // Create transaction inputs from the selected UTXOs.
    let tx_inputs: Vec<TxIn> = selected_utxos
        .iter()
        .map(|(outpoint, _)| TxIn {
            previous_output: *outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::new(),
        })
        .collect();

    // Create transaction outputs.
    let mut outputs = Vec::new();

    // OP_RETURN output with data.
    // let push_bytes = PushBytesBuf::try_from(data.as_bytes().to_vec())?;
    let script = Builder::new()
        .push_opcode(OP_RETURN)
        .push_slice(PushBytesBuf::try_from(prefix.as_bytes().to_vec())?)
        .push_slice(PushBytesBuf::try_from(data.as_bytes().to_vec())?)
        .into_script();

    outputs.push(TxOut {
        value: Amount::from_sat(0),
        script_pubkey: script,
    });
    // Change output (if any).
    let change_amount = input_amount - total_needed;
    if change_amount > Amount::from_sat(0) {
        outputs.push(TxOut {
            value: change_amount,
            script_pubkey: address.script_pubkey(),
        });
    }

    let mut tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: tx_inputs,
        output: outputs,
    };

    let sighash_type = EcdsaSighashType::All;
    let mut cache = SighashCache::new(&mut tx);
    for (i, (_, utxo)) in selected_utxos.iter().enumerate() {
        let sighash = cache
            .p2wpkh_signature_hash(i, &utxo.script_pubkey, utxo.value, sighash_type)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let msg = Message::from(sighash);
        let signature = secp.sign_ecdsa(&msg, &private_key.inner);

        // Create a Bitcoin ECDSA signature with sighash type
        let signature = bitcoin::ecdsa::Signature {
            signature,
            sighash_type,
        };

        // Set the witness using p2wpkh helper
        cache
            .witness_mut(i)
            .ok_or_else(|| anyhow::anyhow!("Failed to get witness"))
            .map(|witness| *witness = Witness::p2wpkh(&signature, &pk))?;
    }

    let tx = cache.into_transaction();
    // --------------------------------

    // Broadcast transaction
    let tx_hex = serialize_hex(&tx);
    let txid = client.broadcast_signed_transaction(&tx_hex).await?;

    info!("Transaction broadcasted with txid: {}", txid);

    Ok(())
}
