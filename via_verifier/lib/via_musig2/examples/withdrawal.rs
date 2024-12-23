use bitcoin::{
    absolute,
    hashes::Hash,
    sighash::{Prevouts, SighashCache},
    taproot::TaprootSpendInfo,
    Address as BitcoinAddress, Amount, Network, OutPoint, TapSighashType, Transaction, TxIn, TxOut,
    Witness, XOnlyPublicKey,
};
// use bitcoincore_rpc::Auth;
use musig2::{
    verify_single, CompactSignature, FirstRound, KeyAggContext, PartialSignature, SecNonceSpices,
};
use rand::{rngs::OsRng, Rng};
use secp256k1_musig2::{PublicKey, Secp256k1, SecretKey};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // -------------------------------------------
    // Setup: Create secret and public keys for three participants
    // -------------------------------------------
    let mut rng = OsRng;
    let secret_key_1 = SecretKey::new(&mut rng);
    let secret_key_2 = SecretKey::new(&mut rng);
    let secret_key_3 = SecretKey::new(&mut rng);

    let secp = Secp256k1::new();
    let public_key_1 = PublicKey::from_secret_key(&secp, &secret_key_1);
    let public_key_2 = PublicKey::from_secret_key(&secp, &secret_key_2);
    let public_key_3 = PublicKey::from_secret_key(&secp, &secret_key_3);

    // -------------------------------------------
    // Key aggregation (MuSig2)
    // -------------------------------------------
    let pubkeys = vec![public_key_1, public_key_2, public_key_3];
    let key_agg_ctx = KeyAggContext::new(pubkeys)?;
    let aggregated_pubkey: PublicKey = key_agg_ctx.aggregated_pubkey();

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
    // NOTE: Update these with real RPC credentials and URL
    let _rpc_url = "http://127.0.0.1:18443";
    let _rpc_user = "user";
    let _rpc_pass = "pass";

    // let client = BitcoinClient::new(rpc_url, Network::Regtest, Auth::UserPass(rpc_user.into(), rpc_pass.into()))?;

    // -------------------------------------------
    // Instead of fetching UTXOs from node, use a fake constant UTXO for demonstration
    // Comment out real fetching code
    // -------------------------------------------
    // let utxos = client.fetch_utxos(&address).await?;
    // if utxos.is_empty() {
    //     eprintln!("No UTXOs found for this address. Please fund it first.");
    //     return Ok(());
    // }

    // We'll use a fake UTXO here
    let fake_utxo_txid = bitcoin::Txid::from_slice(&[0x11; 32]).unwrap(); // just a dummy 32-byte txid
    let fake_vout = 0;
    let fake_utxo_amount = Amount::from_btc(1.0).unwrap(); // 1 BTC in the fake utxo
    let fake_utxo = (fake_utxo_txid, fake_vout, fake_utxo_amount);

    // -------------------------------------------
    // Create a transaction spending this fake UTXO
    // -------------------------------------------
    let send_amount = Amount::from_btc(0.1).unwrap(); // amount to send
    let fee_amount = Amount::from_btc(0.0001).unwrap();
    let change_amount = fake_utxo_amount - send_amount - fee_amount;

    // The recipient address - for demonstration let's send to the same aggregated address
    // or another regtest address.
    let recipient_address = address.clone();
    let change_address = address.clone();

    let txin = TxIn {
        previous_output: OutPoint {
            txid: fake_utxo.0,
            vout: fake_utxo.1,
        },
        sequence: bitcoin::Sequence(0xFFFFFFFF),
        witness: Witness::new(),
        script_sig: bitcoin::Script::new().into(),
    };

    let txout_recipient = TxOut {
        value: send_amount,
        script_pubkey: recipient_address.script_pubkey(),
    };

    let txout_change = TxOut {
        value: change_amount,
        script_pubkey: change_address.script_pubkey(),
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
            value: fake_utxo_amount,
            script_pubkey: recipient_address.script_pubkey(),
        }]),
        sighash_type,
    )?;

    let message = sighash; // This 32-byte hash is what we will sign using MuSig2

    // -------------------------------------------
    // MuSig2 Nonce Exchange and Partial Signatures
    // -------------------------------------------
    // First round: generate public nonces
    let mut first_round_1 = FirstRound::new(
        key_agg_ctx.clone(),
        rand::thread_rng().gen::<[u8; 32]>(),
        0,
        SecNonceSpices::new()
            .with_seckey(secret_key_1)
            .with_message(&message),
    )?;

    let mut first_round_2 = FirstRound::new(
        key_agg_ctx.clone(),
        rand::thread_rng().gen::<[u8; 32]>(),
        1,
        SecNonceSpices::new()
            .with_seckey(secret_key_2)
            .with_message(&message),
    )?;

    let mut first_round_3 = FirstRound::new(
        key_agg_ctx.clone(),
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

    // For a key-path spend in taproot, the witness is just the signature
    unsigned_tx.input[0].witness = Witness::from(vec![final_sig_with_hashtype]);

    // -------------------------------------------
    // Print the signed raw transaction (in hex)
    // -------------------------------------------
    let signed_raw_tx = bitcoin::consensus::encode::serialize_hex(&unsigned_tx);
    println!("Signed raw transaction (hex): {}", signed_raw_tx);

    // NOTE: In real scenario, you could broadcast this transaction using the Bitcoin node RPC:
    // client.broadcast_raw_tx(&signed_raw_tx).await?;
    // Here we just printed it.

    Ok(())
}
