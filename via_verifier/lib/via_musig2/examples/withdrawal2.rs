// SPDX-License-Identifier: CC0-1.0

//! Demonstrate creating a transaction that spends to and from p2tr outputs.

use std::str::FromStr;

use bitcoin::{
    hashes::Hash,
    key::{Keypair, TapTweak, TweakedKeypair},
    locktime::absolute,
    secp256k1::{Message, Secp256k1, SecretKey},
    sighash::{Prevouts, SighashCache, TapSighashType},
    transaction, Address, Amount, Network, PrivateKey, ScriptBuf, Sequence, Transaction, TxIn,
    TxOut, Witness,
};
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
    let private_key =
        PrivateKey::from_wif("cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R").unwrap();

    let secret_key = SecretKey::from_slice(&private_key.inner.secret_bytes()).unwrap();

    let keypair = Keypair::from_secret_key(&secp, &secret_key);

    let (internal_key, _parity) = keypair.x_only_public_key();

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

    // Sign the sighash using the secp256k1 library (exported by rust-bitcoin).
    let tweaked: TweakedKeypair = keypair.tap_tweak(&secp, None);
    let msg = Message::from_digest_slice(&sighash.to_byte_array()).expect("32 bytes");
    let signature = secp.sign_schnorr(&msg, &tweaked.to_inner());

    // Update the witness stack.
    let signature = bitcoin::taproot::Signature {
        signature,
        sighash_type,
    };
    *sighasher.witness_mut(input_index).unwrap() = Witness::p2tr_key_spend(&signature);

    // Get the signed transaction.
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
