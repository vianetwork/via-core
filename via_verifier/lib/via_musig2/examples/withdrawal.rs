// SPDX-License-Identifier: CC0-1.0

//! Demonstrate creating a transaction that spends to and from p2tr outputs with musig2.

use std::str::FromStr;

use bitcoin::{
    hashes::Hash,
    key::Keypair,
    locktime::absolute,
    secp256k1::{Secp256k1, SecretKey},
    sighash::{Prevouts, SighashCache, TapSighashType},
    transaction, Address, Amount, Network, PrivateKey, ScriptBuf, Sequence, TapTweakHash,
    Transaction, TxIn, TxOut, Witness,
};
use musig2::{secp::Scalar, KeyAggContext};
use rand::Rng;
use secp256k1_musig2::schnorr::Signature;
use via_btc_client::{inscriber::Inscriber, types::NodeAuth};

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: Network = Network::Regtest;
const PK: &str = "cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm";
const SPEND_AMOUNT: Amount = Amount::from_sat(5_000_000);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secp = Secp256k1::new();

    // Get a keypair we control. In a real application these would come from a stored secret.
    let private_key_1 =
        PrivateKey::from_wif("cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R").unwrap();

    let private_key_2 =
        PrivateKey::from_wif("cUWA5dZXc6NwLovW3Kr9DykfY5ysFigKZM5Annzty7J8a43Fe2YF").unwrap();

    let private_key_3 =
        PrivateKey::from_wif("cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm").unwrap();

    let secret_key_1 = SecretKey::from_slice(&private_key_1.inner.secret_bytes()).unwrap();
    let secret_key_2 = SecretKey::from_slice(&private_key_2.inner.secret_bytes()).unwrap();
    let secret_key_3 = SecretKey::from_slice(&private_key_3.inner.secret_bytes()).unwrap();

    let keypair_1 = Keypair::from_secret_key(&secp, &secret_key_1);
    let keypair_2 = Keypair::from_secret_key(&secp, &secret_key_2);
    let keypair_3 = Keypair::from_secret_key(&secp, &secret_key_3);

    let (internal_key_1, parity_1) = keypair_1.x_only_public_key();
    let (internal_key_2, parity_2) = keypair_2.x_only_public_key();
    let (internal_key_3, parity_3) = keypair_3.x_only_public_key();

    // -------------------------------------------
    // Key aggregation (MuSig2)
    // -------------------------------------------
    let pubkeys = vec![
        musig2::secp256k1::PublicKey::from_slice(&internal_key_1.public_key(parity_1).serialize())
            .unwrap(),
        musig2::secp256k1::PublicKey::from_slice(&internal_key_2.public_key(parity_2).serialize())
            .unwrap(),
        musig2::secp256k1::PublicKey::from_slice(&internal_key_3.public_key(parity_3).serialize())
            .unwrap(),
    ];

    let mut musig_key_agg_cache = KeyAggContext::new(pubkeys)?;

    let agg_pubkey = musig_key_agg_cache.aggregated_pubkey::<secp256k1_musig2::PublicKey>();
    let (xonly_agg_key, _) = agg_pubkey.x_only_public_key();

    // Convert to bitcoin XOnlyPublicKey first
    let internal_key = bitcoin::XOnlyPublicKey::from_slice(&xonly_agg_key.serialize())?;

    // Calculate taproot tweak
    let tap_tweak = TapTweakHash::from_key_and_tweak(internal_key, None);
    let tweak = tap_tweak.to_scalar();
    let tweak_bytes = tweak.to_be_bytes();
    let tweak = secp256k1_musig2::Scalar::from_be_bytes(tweak_bytes).unwrap();

    // Apply tweak to the key aggregation context before signing
    musig_key_agg_cache = musig_key_agg_cache.with_xonly_tweak(tweak)?;

    // Use internal_key for address creation
    let address = Address::p2tr(&secp, internal_key, None, NETWORK);

    println!("address: {}", address);

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

    // Get an unspent output that is locked to the key above that we control.
    // In a real application these would come from the chain.
    let (dummy_out_point, dummy_utxo) = utxos[0].clone();

    let change_amount = dummy_utxo.value - SPEND_AMOUNT;

    // Get an address to send to.
    let address = receivers_address();

    // The input for the transaction we are constructing.
    let input = TxIn {
        previous_output: dummy_out_point, // The dummy output we are spending.
        script_sig: ScriptBuf::default(), // For a p2tr script_sig is empty.
        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
        witness: Witness::default(), // Filled in after signing.
    };

    // The spend output is locked to a key controlled by the receiver.
    let spend = TxOut {
        value: SPEND_AMOUNT,
        script_pubkey: address.script_pubkey(),
    };

    // The change output is locked to a key controlled by us.
    let change = TxOut {
        value: change_amount,
        script_pubkey: ScriptBuf::new_p2tr(&secp, internal_key, None), // Change comes back to us.
    };

    // The transaction we want to sign and broadcast.
    let mut unsigned_tx = Transaction {
        version: transaction::Version::TWO,  // Post BIP-68.
        lock_time: absolute::LockTime::ZERO, // Ignore the locktime.
        input: vec![input],                  // Input goes into index 0.
        output: vec![spend, change],         // Outputs, order does not matter.
    };
    let input_index = 0;

    // Get the sighash to sign.
    let sighash_type = TapSighashType::Default;
    let prevouts = vec![dummy_utxo];
    let prevouts = Prevouts::All(&prevouts);

    let mut sighasher = SighashCache::new(&mut unsigned_tx);
    let sighash = sighasher
        .taproot_key_spend_signature_hash(input_index, &prevouts, sighash_type)
        .expect("failed to construct sighash");

    // -------------------------------------------
    // MuSig2 Signing Process
    // -------------------------------------------
    use musig2::{FirstRound, SecNonceSpices};
    use rand::thread_rng;

    // Convert bitcoin::SecretKey to musig2::SecretKey for each participant
    let secret_key_1 = musig2::secp256k1::SecretKey::from_slice(&secret_key_1[..]).unwrap();
    let secret_key_2 = musig2::secp256k1::SecretKey::from_slice(&secret_key_2[..]).unwrap();
    let secret_key_3 = musig2::secp256k1::SecretKey::from_slice(&secret_key_3[..]).unwrap();

    // First round: Generate nonces
    let mut first_round_1 = FirstRound::new(
        musig_key_agg_cache.clone(), // Use tweaked context
        thread_rng().gen::<[u8; 32]>(),
        0,
        SecNonceSpices::new()
            .with_seckey(secret_key_1)
            .with_message(&sighash.to_byte_array()),
    )?;

    let mut first_round_2 = FirstRound::new(
        musig_key_agg_cache.clone(),
        thread_rng().gen::<[u8; 32]>(),
        1,
        SecNonceSpices::new()
            .with_seckey(secret_key_2)
            .with_message(&sighash.to_byte_array()),
    )?;

    let mut first_round_3 = FirstRound::new(
        musig_key_agg_cache.clone(),
        thread_rng().gen::<[u8; 32]>(),
        2,
        SecNonceSpices::new()
            .with_seckey(secret_key_3)
            .with_message(&sighash.to_byte_array()),
    )?;

    // Exchange nonces
    let nonce_1 = first_round_1.our_public_nonce();
    let nonce_2 = first_round_2.our_public_nonce();
    let nonce_3 = first_round_3.our_public_nonce();

    first_round_1.receive_nonce(1, nonce_2.clone())?;
    first_round_1.receive_nonce(2, nonce_3.clone())?;
    first_round_2.receive_nonce(0, nonce_1.clone())?;
    first_round_2.receive_nonce(2, nonce_3.clone())?;
    first_round_3.receive_nonce(0, nonce_1.clone())?;
    first_round_3.receive_nonce(1, nonce_2.clone())?;

    // Second round: Create partial signatures
    let binding = sighash.to_byte_array();
    let mut second_round_1 = first_round_1.finalize(secret_key_1, &binding)?;
    let binding = sighash.to_byte_array();
    let second_round_2 = first_round_2.finalize(secret_key_2, &binding)?;
    let binding = sighash.to_byte_array();
    let second_round_3 = first_round_3.finalize(secret_key_3, &binding)?;
    // Combine partial signatures
    let partial_sig_2: [u8; 32] = second_round_2.our_signature();
    let partial_sig_3: [u8; 32] = second_round_3.our_signature();

    second_round_1.receive_signature(1, Scalar::from_slice(&partial_sig_2).unwrap())?;
    second_round_1.receive_signature(2, Scalar::from_slice(&partial_sig_3).unwrap())?;

    let final_signature: Signature = second_round_1.finalize()?;

    // Update the witness stack with the aggregated signature
    let signature = bitcoin::taproot::Signature {
        signature: bitcoin::secp256k1::schnorr::Signature::from_slice(
            &final_signature.to_byte_array(),
        )?,
        sighash_type,
    };
    *sighasher.witness_mut(input_index).unwrap() = Witness::p2tr_key_spend(&signature);

    // Get the signed transaction
    let tx = sighasher.into_transaction();

    // BOOM! Transaction signed and ready to broadcast.
    println!("{:#?}", tx);

    let tx_hex = bitcoin::consensus::encode::serialize_hex(&tx);
    let res = client.broadcast_signed_transaction(&tx_hex).await?;
    println!("res: {:?}", res);

    Ok(())
}

/// A dummy address for the receiver.
///
/// We lock the spend output to the key associated with this address.
///
/// (FWIW this is an arbitrary mainnet address from block 805222.)
fn receivers_address() -> Address {
    Address::from_str("bc1p0dq0tzg2r780hldthn5mrznmpxsxc0jux5f20fwj0z3wqxxk6fpqm7q0va")
        .expect("a valid address")
        .require_network(Network::Bitcoin)
        .expect("valid address for mainnet")
}
