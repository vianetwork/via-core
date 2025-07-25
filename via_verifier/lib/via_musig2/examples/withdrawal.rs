// SPDX-License-Identifier: CC0-1.0

//! Demonstrate creating a transaction that spends to and from p2tr outputs with musig2.

use std::{str::FromStr, sync::Arc};

use bitcoin::{
    hashes::Hash,
    hex::{Case, DisplayHex},
    key::Keypair,
    policy::MAX_STANDARD_TX_WEIGHT,
    secp256k1::{Secp256k1, SecretKey},
    sighash::TapSighashType,
    Address, Amount, Network, PrivateKey, TapTweakHash, TxOut, Txid, Witness,
};
use musig2::KeyAggContext;
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps, types::NodeAuth};
use via_musig2::{
    fee::WithdrawalFeeStrategy, get_signer, transaction_builder::TransactionBuilder,
    verify_signature,
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: Network = Network::Regtest;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secp = Secp256k1::new();

    // Get a keypair we control. In a real application these would come from a stored secret.
    let pk_str_1 = "cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R";
    let pk_str_2 = "cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm";

    let private_key_1 = PrivateKey::from_wif(pk_str_1).unwrap();
    let private_key_2 = PrivateKey::from_wif(pk_str_2).unwrap();

    let secret_key_1 = SecretKey::from_slice(&private_key_1.inner.secret_bytes()).unwrap();
    let secret_key_2 = SecretKey::from_slice(&private_key_2.inner.secret_bytes()).unwrap();

    let keypair_1 = Keypair::from_secret_key(&secp, &secret_key_1);
    let keypair_2 = Keypair::from_secret_key(&secp, &secret_key_2);

    let (internal_key_1, parity_1) = keypair_1.x_only_public_key();
    let (internal_key_2, parity_2) = keypair_2.x_only_public_key();

    // -------------------------------------------
    // Key aggregation (MuSig2)
    // -------------------------------------------
    let pubkeys_str = vec![
        internal_key_1
            .public_key(parity_1)
            .serialize()
            .to_hex_string(Case::Lower),
        internal_key_2
            .public_key(parity_2)
            .serialize()
            .to_hex_string(Case::Lower),
    ];
    let pubkeys = vec![
        musig2::secp256k1::PublicKey::from_slice(&internal_key_1.public_key(parity_1).serialize())
            .unwrap(),
        musig2::secp256k1::PublicKey::from_slice(&internal_key_2.public_key(parity_2).serialize())
            .unwrap(),
    ];

    let mut signer1 = get_signer(pk_str_1, pubkeys_str.clone())?;
    let mut signer2 = get_signer(pk_str_2, pubkeys_str.clone())?;

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
    let auth = NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string());
    let config = ViaBtcClientConfig {
        network: NETWORK.to_string(),
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };

    let btc_client = BitcoinClient::new(RPC_URL, auth, config).unwrap();

    // -------------------------------------------
    // Fetching UTXOs from node
    // -------------------------------------------
    let receivers_address = receivers_address();

    // Create outputs for grouped withdrawals
    let outputs: Vec<TxOut> = vec![
        TxOut {
            value: Amount::from_sat(150000000),
            script_pubkey: receivers_address.script_pubkey(),
        },
        TxOut {
            value: Amount::from_sat(40000000),
            script_pubkey: address.script_pubkey(),
        },
    ];

    let op_return_prefix: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL:";
    let op_return_data = Txid::all_zeros().to_byte_array();

    let tx_builder = TransactionBuilder::new(Arc::new(btc_client.clone()), address.clone())?;
    let mut unsigned_tx = tx_builder
        .build_transaction_with_op_return(
            outputs,
            op_return_prefix,
            vec![&op_return_data.to_vec()],
            Arc::new(WithdrawalFeeStrategy::new()),
            None,
            None,
            MAX_STANDARD_TX_WEIGHT as u64,
        )
        .await?[0]
        .clone();

    let messages = tx_builder.get_tr_sighashes(&unsigned_tx)?;
    let message1 = messages[0].clone();
    let message2 = messages[1].clone();

    unsigned_tx.utxos.iter().for_each(|utxo| {
        println!(
            "{:?}",
            utxo.1.script_pubkey == address.clone().script_pubkey()
        )
    });

    // -------------------------------------------
    // MuSig2 Signing Process
    // -------------------------------------------

    signer1.start_signing_session(message1.clone())?;
    signer2.start_signing_session(message1.clone())?;

    let nonce1 = signer1.our_nonce().unwrap();
    let nonce2 = signer2.our_nonce().unwrap();

    signer1.mark_nonce_submitted();
    signer2.mark_nonce_submitted();

    signer1.receive_nonce(1, nonce2.clone())?;
    signer2.receive_nonce(0, nonce1.clone())?;

    signer1.create_partial_signature()?;
    let partial_sig2 = signer2.create_partial_signature()?;

    signer1.mark_partial_sig_submitted();
    signer2.mark_partial_sig_submitted();

    signer1.receive_partial_signature(1, partial_sig2)?;

    let musig2_signature1 = signer1.create_final_signature()?;

    let mut signer1 = get_signer(pk_str_1, pubkeys_str.clone())?;
    let mut signer2 = get_signer(pk_str_2, pubkeys_str.clone())?;

    signer1.start_signing_session(message2.clone())?;
    signer2.start_signing_session(message2.clone())?;

    let nonce1 = signer1.our_nonce().unwrap();
    let nonce2 = signer2.our_nonce().unwrap();

    signer1.mark_nonce_submitted();
    signer2.mark_nonce_submitted();

    signer1.receive_nonce(1, nonce2.clone())?;
    signer2.receive_nonce(0, nonce1.clone())?;

    signer1.create_partial_signature()?;
    let partial_sig2 = signer2.create_partial_signature()?;

    signer1.mark_partial_sig_submitted();
    signer2.mark_partial_sig_submitted();

    signer1.receive_partial_signature(1, partial_sig2)?;

    let musig2_signature2 = signer1.create_final_signature()?;

    let mut final_sig_with_hashtype1 = musig2_signature1.serialize().to_vec();
    let mut final_sig_with_hashtype2 = musig2_signature2.serialize().to_vec();

    let sighash_type = TapSighashType::All;
    final_sig_with_hashtype1.push(sighash_type as u8);
    final_sig_with_hashtype2.push(sighash_type as u8);

    unsigned_tx.tx.input[0].witness = Witness::from(vec![final_sig_with_hashtype1.clone()]);
    unsigned_tx.tx.input[1].witness = Witness::from(vec![final_sig_with_hashtype2.clone()]);

    let tx_hex = bitcoin::consensus::encode::serialize_hex(&unsigned_tx.tx);
    let agg_pub = signer1.aggregated_pubkey();

    verify_signature(agg_pub.clone(), musig2_signature1, &message1)?;
    verify_signature(agg_pub, musig2_signature2, &message2)?;

    let res = btc_client.broadcast_signed_transaction(&tx_hex).await?;
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
