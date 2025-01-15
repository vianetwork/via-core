use std::str::FromStr;

use bitcoin::{
    absolute,
    hashes::Hash,
    secp256k1,
    secp256k1::schnorr,
    sighash::{Prevouts, SighashCache},
    taproot::{TaprootBuilder, TaprootSpendInfo},
    Address as BitcoinAddress, Amount, CompressedPublicKey, Network, PrivateKey, ScriptBuf,
    TapSighashType, Transaction, TxIn, TxOut, Witness, XOnlyPublicKey,
};
use musig2::{
    verify_single, CompactSignature, FirstRound, KeyAggContext, PartialSignature, SecNonceSpices,
};
use rand::Rng;
use secp256k1_musig2::{PublicKey, Scalar, Secp256k1, SecretKey};
use via_btc_client::{inscriber::Inscriber, types::NodeAuth};

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: Network = Network::Regtest;
const PK: &str = "cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm";

// See https://github.com/bitcoin/bips/blob/master/bip-0341.mediawiki#constructing-and-spending-taproot-outputs
use std::sync::LazyLock;

static UNSPENDABLE_XONLY_PUBKEY: LazyLock<bitcoin::secp256k1::XOnlyPublicKey> =
    LazyLock::new(|| {
        XOnlyPublicKey::from_str("93c7378d96518a75448821c4f7c8f4bae7ce60f804d03d1f0628dd5dd0f5de51")
            .unwrap()
    });

static SECP: LazyLock<secp256k1::Secp256k1<secp256k1::All>> =
    LazyLock::new(|| secp256k1::Secp256k1::new());

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // -------------------------------------------
    // Setup: Create secret and public keys for three participants
    // -------------------------------------------

    let private_key_1 =
        PrivateKey::from_wif("cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R")?;
    let private_key_2 =
        PrivateKey::from_wif("cUWA5dZXc6NwLovW3Kr9DykfY5ysFigKZM5Annzty7J8a43Fe2YF")?;
    let private_key_3 =
        PrivateKey::from_wif("cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm")?;

    let secp = Secp256k1::new();

    let secret_key_1 = SecretKey::from_slice(&private_key_1.inner.secret_bytes())?;
    let public_key_1: PublicKey = secret_key_1.public_key(&secp);

    let secret_key_2 = SecretKey::from_slice(&private_key_2.inner.secret_bytes())?;
    let public_key_2: PublicKey = secret_key_2.public_key(&secp);

    let secret_key_3 = SecretKey::from_slice(&private_key_3.inner.secret_bytes())?;
    let public_key_3: PublicKey = secret_key_3.public_key(&secp);

    // -------------------------------------------
    // Create receiver addresses
    // -------------------------------------------
    let com_public_key_1 = CompressedPublicKey::from_slice(&public_key_1.serialize().to_vec())?;
    let address_1 = BitcoinAddress::p2wpkh(&com_public_key_1, Network::Regtest);

    // -------------------------------------------
    // Key aggregation (MuSig2)
    // -------------------------------------------
    let pubkeys = vec![public_key_1, public_key_2, public_key_3];
    let mut musig_key_agg_cache = KeyAggContext::new(pubkeys)?;

    let tree_info = create_taproot_spend_info(Some(agg_xonly_pubkey_raw), scripts)?;

    let plain_tweak: [u8; 32] = *b"this could be a BIP32 tweak....\0";
    let xonly_tweak: [u8; 32] = *b"this could be a Taproot tweak..\0";

    let plain_tweak = Scalar::from_be_bytes(plain_tweak).unwrap();
    let xonly_tweak = Scalar::from_be_bytes(xonly_tweak).unwrap();

    musig_key_agg_cache = musig_key_agg_cache.with_plain_tweak(plain_tweak)?;
    musig_key_agg_cache = musig_key_agg_cache.with_xonly_tweak(xonly_tweak)?;
    musig_key_agg_cache.with_tweak(tweak, is_xonly);

    let aggregated_pubkey: PublicKey = musig_key_agg_cache.aggregated_pubkey();

    // Convert to x-only pubkey for Taproot address
    let (xonly_agg_key, _parity) = aggregated_pubkey.x_only_public_key();
    let xonly_pub = XOnlyPublicKey::from_slice(&xonly_agg_key.serialize())?;
    // Create a P2TR address from the aggregated x-only public key
    let secp_btc = bitcoin::secp256k1::Secp256k1::new();
    let tap_info = TaprootSpendInfo::new_key_spend(&secp_btc, xonly_pub, None);
    let tweaked_key = tap_info.output_key();
    let address = BitcoinAddress::p2tr(&secp_btc, tweaked_key.into(), None, Network::Regtest);
    println!("Aggregated taproot address: {}", address);

    // -------------------------------------------
    // Connect to Bitcoin node (adjust RPC credentials and URL)
    // -------------------------------------------
    let inscriber = Inscriber::new(
        RPC_URL,
        NETWORK,
        NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string()),
        PK,
        None,
    )
    .await?;
    let client = inscriber.get_client().await;

    // -------------------------------------------
    // Fetching UTXOs from node
    // -------------------------------------------
    let utxos = client.fetch_utxos(&address).await?;

    // -------------------------------------------
    // Create a transaction spending the UTXO
    // -------------------------------------------
    let send_amount = Amount::from_btc(0.1).unwrap();
    let fee_amount = Amount::from_btc(0.0001).unwrap();
    let change_amount = utxos[0].1.value - send_amount - fee_amount;

    let txin = TxIn {
        previous_output: utxos[0].0,
        sequence: bitcoin::Sequence(0xFFFFFFFF),
        witness: Witness::new(),
        script_sig: bitcoin::Script::new().into(),
    };

    let txout_recipient = TxOut {
        value: send_amount,
        script_pubkey: address_1.script_pubkey(),
    };

    let txout_change = TxOut {
        value: change_amount,
        script_pubkey: address.script_pubkey(),
    };

    let mut unsigned_tx = Transaction {
        version: bitcoin::transaction::Version(2),
        lock_time: absolute::LockTime::ZERO,
        input: vec![txin],
        output: vec![txout_recipient, txout_change],
    };

    // -------------------------------------------
    // Compute the BIP341 sighash for signing
    // -------------------------------------------
    let mut sighash_cache = SighashCache::new(&unsigned_tx);
    let sighash_type = TapSighashType::All;

    // For taproot key spend (no script):
    let sighash = sighash_cache.taproot_key_spend_signature_hash(
        0,
        &Prevouts::All(&[TxOut {
            value: utxos[0].1.value,
            script_pubkey: utxos[0].1.script_pubkey.clone(),
        }]),
        sighash_type,
    )?;

    let message = sighash; // This 32-byte hash is what we will sign using MuSig2

    // -------------------------------------------
    // MuSig2 Nonce Exchange and Partial Signatures
    // -------------------------------------------
    // First round: generate public nonces
    let mut first_round_1 = FirstRound::new(
        musig_key_agg_cache.clone(),
        rand::thread_rng().gen::<[u8; 32]>(),
        0,
        SecNonceSpices::new()
            .with_seckey(secret_key_1)
            .with_message(&message),
    )?;

    let mut first_round_2 = FirstRound::new(
        musig_key_agg_cache.clone(),
        rand::thread_rng().gen::<[u8; 32]>(),
        1,
        SecNonceSpices::new()
            .with_seckey(secret_key_2)
            .with_message(&message),
    )?;

    let mut first_round_3 = FirstRound::new(
        musig_key_agg_cache.clone(),
        rand::thread_rng().gen::<[u8; 32]>(),
        2,
        SecNonceSpices::new()
            .with_seckey(secret_key_3)
            .with_message(&message),
    )?;

    // Get public nonces
    let pub_nonce_1 = first_round_1.our_public_nonce();
    let pub_nonce_2 = first_round_2.our_public_nonce();
    let pub_nonce_3 = first_round_3.our_public_nonce();

    // Exchange nonces between participants
    first_round_1.receive_nonce(1, pub_nonce_2.clone())?;
    first_round_1.receive_nonce(2, pub_nonce_3.clone())?;

    first_round_2.receive_nonce(0, pub_nonce_1.clone())?;
    first_round_2.receive_nonce(2, pub_nonce_3.clone())?;

    first_round_3.receive_nonce(0, pub_nonce_1.clone())?;
    first_round_3.receive_nonce(1, pub_nonce_2.clone())?;

    // Second round: Create partial signatures
    let mut second_round_1 = first_round_1.finalize(secret_key_1, &message)?;
    let second_round_2 = first_round_2.finalize(secret_key_2, &message)?;
    let second_round_3 = first_round_3.finalize(secret_key_3, &message)?;

    // Get partial signatures
    let _partial_sig_1: PartialSignature = second_round_1.our_signature();
    let partial_sig_2: PartialSignature = second_round_2.our_signature();
    let partial_sig_3: PartialSignature = second_round_3.our_signature();

    // One participant collects all partial signatures
    second_round_1.receive_signature(1, partial_sig_2)?;
    second_round_1.receive_signature(2, partial_sig_3)?;

    // Final aggregate MuSig2 signature
    let final_signature: CompactSignature = second_round_1.finalize()?;

    // Verify the signature (optional sanity check)
    match verify_single(aggregated_pubkey, final_signature, message) {
        Ok(_) => println!("MuSig2 signature verified successfully!"),
        Err(e) => println!("MuSig2 signature verification failed: {:?}", e),
    }

    // -------------------------------------------
    // Insert the final signature into the transaction witness
    // -------------------------------------------
    let mut final_sig_with_hashtype = final_signature.serialize().to_vec();
    final_sig_with_hashtype.push(sighash_type as u8); // For SIGHASH_DEFAULT this is 0x00

    let signature = bitcoin::taproot::Signature {
        sighash_type,
        signature: schnorr::Signature::from_slice(&final_signature.serialize())?,
    };

    // For a key-path spend in taproot, the witness is just the signature
    unsigned_tx.input[0].witness.push(signature.to_vec());

    let secp = Secp256k1::new();

    // -------------------------------------------
    // Verify the Schnorr signature
    // -------------------------------------------
    let array: [u8; 64] = final_signature.serialize().try_into().unwrap();
    let sig = schnorr::Signature::from_slice(&array)?;

    // match secp.verify_schnorr(
    //     &sig,
    //     message.as_byte_array(),
    //     &aggregated_pubkey.x_only_public_key().0,
    // ) {
    //     Ok(_) => println!("Valid schnorr sig!"),
    //     Err(e) => {
    //         println!("Invalid schnorr sig: {:?}", e);
    //         return Ok(());
    //     }
    // }

    // -------------------------------------------
    // Print the signed raw transaction (in hex)
    // -------------------------------------------
    let signed_raw_tx = bitcoin::consensus::encode::serialize_hex(&unsigned_tx);
    println!("Signed raw transaction (hex): {}", signed_raw_tx);

    // -------------------------------------------
    // Broadcast the signed transation
    // -------------------------------------------
    client.broadcast_signed_transaction(&signed_raw_tx).await?;

    Ok(())
}

fn create_taproot_spend_info(
    internal_key: Option<XOnlyPublicKey>,
    scripts: Vec<ScriptBuf>,
) -> anyhow::Result<TaprootSpendInfo> {
    let n = scripts.len();
    if n == 0 {
        return Err(anyhow::anyhow!("No scripts provided"));
    }

    let taproot_builder = if n > 1 {
        let m: u8 = ((n - 1).ilog2() + 1) as u8; // m = ceil(log(n))
        let k = 2_usize.pow(m.into()) - n;
        (0..n).try_fold(TaprootBuilder::new(), |acc, i| {
            acc.add_leaf(m - ((i >= n - k) as u8), scripts[i].clone())
        })?
    } else {
        TaprootBuilder::new().add_leaf(0, scripts[0].clone())?
    };
    let tree_info = match internal_key {
        Some(xonly_pk) => taproot_builder.finalize(&SECP, xonly_pk).map_err(|e| e)?,
        None => taproot_builder
            .finalize(&SECP, *UNSPENDABLE_XONLY_PUBKEY)
            .map_err(|e| e)?,
    };
    Ok(tree_info)
}
