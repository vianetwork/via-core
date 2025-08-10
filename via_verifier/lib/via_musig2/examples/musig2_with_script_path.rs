use std::{str::FromStr, sync::Arc, time::Duration};

use bitcoin::{
    absolute::LockTime,
    blockdata::{opcodes::all::*, script::Builder},
    hashes::Hash,
    hex::{Case, DisplayHex},
    policy::MAX_STANDARD_TX_WEIGHT,
    secp256k1::{self, Keypair, Secp256k1, SecretKey},
    sighash::{Prevouts, SighashCache},
    taproot::{LeafVersion, TaprootBuilder, TaprootSpendInfo},
    transaction::Version,
    Address, Amount, Network, OutPoint, PrivateKey, ScriptBuf, Sequence, TapLeafHash,
    TapSighashType, Transaction, TxIn, TxOut, Txid, Witness, XOnlyPublicKey,
};
use musig2::KeyAggContext;
use tokio::time::sleep;
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps, types::NodeAuth};
use via_musig2::{
    fee::WithdrawalFeeStrategy, get_signer_with_merkle_root,
    transaction_builder::TransactionBuilder, verify_signature, Signer,
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secp = Secp256k1::new();

    // --- MuSig2 setup with 2 keys (unchanged for internal key aggregation) ---
    let pk_str_1 = "cQnW8oDqEME4gxJHC4MC9HvJECcF7Ju8oanWdjWLGxDbkfWo7vZa";
    let pk_str_2 = "cVJYEHTzmfdRPoX6fL3vRnZVmqy4D1sWaT5WL9U25oZhQktoeHgo";

    let private_key_1 = PrivateKey::from_wif(pk_str_1)?;
    let private_key_2 = PrivateKey::from_wif(pk_str_2)?;

    let secret_key_1 = SecretKey::from_slice(&private_key_1.inner.secret_bytes())?;
    let secret_key_2 = SecretKey::from_slice(&private_key_2.inner.secret_bytes())?;

    let keypair_1 = Keypair::from_secret_key(&secp, &secret_key_1);
    let keypair_2 = Keypair::from_secret_key(&secp, &secret_key_2);

    let (internal_key_1, parity_1) = keypair_1.x_only_public_key();
    let (internal_key_2, parity_2) = keypair_2.x_only_public_key();

    let pubkeys = vec![
        musig2::secp256k1::PublicKey::from_slice(&internal_key_1.public_key(parity_1).serialize())?,
        musig2::secp256k1::PublicKey::from_slice(&internal_key_2.public_key(parity_2).serialize())?,
    ];

    let musig_key_agg_cache = KeyAggContext::new(pubkeys)?;
    let agg_pubkey = musig_key_agg_cache.aggregated_pubkey::<secp256k1_musig2::PublicKey>();

    let (xonly_agg_key, _) = agg_pubkey.x_only_public_key();
    let internal_key = XOnlyPublicKey::from_slice(&xonly_agg_key.serialize())?;

    // --- Governance keys (3-of-2 multisig) ---
    let gov_pk_str_1 = "cQnW8oDqEME4gxJHC4MC9HvJECcF7Ju8oanWdjWLGxDbkfWo7vZa";
    let gov_pk_str_2 = "cVJYEHTzmfdRPoX6fL3vRnZVmqy4D1sWaT5WL9U25oZhQktoeHgo";
    let gov_pk_str_3 = "cPytijNj4VAnczJD5a21bboiPavYDCLmM9AW6cmjUxrDUYnJXQaf";

    let gov_private_key_1 = PrivateKey::from_wif(gov_pk_str_1)?;
    let gov_private_key_2 = PrivateKey::from_wif(gov_pk_str_2)?;
    let gov_private_key_3 = PrivateKey::from_wif(gov_pk_str_3)?;

    // Convert to Schnorr-capable keypairs
    let gov_kp_1 = Keypair::from_secret_key(
        &secp,
        &SecretKey::from_slice(&gov_private_key_1.inner.secret_bytes())?,
    );
    let gov_kp_2 = Keypair::from_secret_key(
        &secp,
        &SecretKey::from_slice(&gov_private_key_2.inner.secret_bytes())?,
    );
    let gov_kp_3 = Keypair::from_secret_key(
        &secp,
        &SecretKey::from_slice(&gov_private_key_3.inner.secret_bytes())?,
    );

    let (gov_x1, _) = gov_kp_1.x_only_public_key();
    let (gov_x2, _) = gov_kp_2.x_only_public_key();
    let (gov_x3, _) = gov_kp_3.x_only_public_key();

    println!(
        "gov_kp_1: {:?}",
        &gov_kp_1.public_key().serialize().to_hex_string(Case::Lower)
    );
    println!(
        "gov_kp_2: {:?}",
        &gov_kp_2.public_key().serialize().to_hex_string(Case::Lower)
    );
    println!(
        "gov_kp_3: {:?}",
        &gov_kp_3.public_key().serialize().to_hex_string(Case::Lower)
    );

    // --- Build Taproot-native 2-of-3 Schnorr multisig script ---
    let multisig_script = Builder::new()
        .push_x_only_key(&gov_x1)
        .push_opcode(OP_CHECKSIG)
        .push_x_only_key(&gov_x2)
        .push_opcode(OP_CHECKSIGADD)
        .push_x_only_key(&gov_x3)
        .push_opcode(OP_CHECKSIGADD)
        .push_int(2)
        .push_opcode(OP_NUMEQUAL)
        .into_script();

    let spend_info = TaprootBuilder::new()
        .add_leaf(0, ScriptBuf::from(multisig_script.clone()))
        .unwrap()
        .finalize(&secp, internal_key)
        .unwrap();

    let taproot_output_key = spend_info.output_key();
    let taproot_address = Address::p2tr_tweaked(taproot_output_key, Network::Regtest);

    println!("spend_info.merkle_root(): {:?}", &spend_info.merkle_root());

    println!("Final Taproot 2-of-3 Schnorr address: {}", taproot_address);

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

    let signer1 =
        get_signer_with_merkle_root(pk_str_1, pubkeys_str.clone(), spend_info.merkle_root())?;
    let signer2 =
        get_signer_with_merkle_root(pk_str_2, pubkeys_str.clone(), spend_info.merkle_root())?;

    // --- RPC setup ---
    let auth = NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string());
    let config = ViaBtcClientConfig {
        network: Network::Regtest.to_string(),
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };

    let btc_client = BitcoinClient::new(RPC_URL, auth, config).unwrap();

    let utxos = btc_client.fetch_utxos(&taproot_address).await?;
    let utxo = utxos[0].clone();

    let tx_hex = transfer_utxo_from_bridge_address_using_governance_wallet_using_script_path(
        &spend_info,
        multisig_script,
        gov_kp_1,
        gov_kp_2,
        gov_x1,
        gov_x2,
        utxo,
    )
    .await?;

    let txid = btc_client.broadcast_signed_transaction(&tx_hex).await?;
    println!(
        "Broadcasted transfer UTXO to GOV wallet using script path, txid: {:?}",
        txid
    );

    sleep(Duration::from_secs(1)).await;

    let tx_hex =
        process_withdraw_using_key_hash(btc_client.clone(), signer1, signer2, taproot_address)
            .await?;

    let txid = btc_client.broadcast_signed_transaction(&tx_hex).await?;
    println!(
        "Broadcasted process withdrawal using the script hash, txid: {:?}",
        txid
    );

    Ok(())
}

async fn process_withdraw_using_key_hash(
    btc_client: BitcoinClient,
    mut signer1: Signer,
    mut signer2: Signer,
    taproot_address: Address,
) -> anyhow::Result<String> {
    // -------------------------------------------
    // Fetching UTXOs from node
    // -------------------------------------------
    let receivers_address = receivers_address();

    // Create outputs for grouped withdrawals
    let outputs: Vec<TxOut> = vec![TxOut {
        value: Amount::from_sat(50000000),
        script_pubkey: receivers_address.script_pubkey(),
    }];

    let op_return_prefix: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL:";
    let op_return_data = Txid::all_zeros().to_byte_array();

    let tx_builder =
        TransactionBuilder::new(Arc::new(btc_client.clone()), taproot_address.clone())?;
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
    assert_eq!(messages.len(), 1);

    let message1 = messages[0].clone();

    unsigned_tx.utxos.iter().for_each(|utxo| {
        println!(
            "{:?}",
            utxo.1.script_pubkey == taproot_address.clone().script_pubkey()
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

    let mut final_sig_with_hashtype1 = musig2_signature1.serialize().to_vec();

    let sighash_type = TapSighashType::All;
    final_sig_with_hashtype1.push(sighash_type as u8);
    println!("final_sig_with_hashtype1 {:?}", &final_sig_with_hashtype1);

    unsigned_tx.tx.input[0].witness = Witness::from(vec![final_sig_with_hashtype1.clone()]);

    let tx_hex = bitcoin::consensus::encode::serialize_hex(&unsigned_tx.tx);
    let agg_pub = signer1.aggregated_pubkey();

    verify_signature(agg_pub.clone(), musig2_signature1, &message1)?;

    Ok(tx_hex)
}

async fn transfer_utxo_from_bridge_address_using_governance_wallet_using_script_path(
    spend_info: &TaprootSpendInfo,
    multisig_script: ScriptBuf,
    gov_kp_1: Keypair,
    gov_kp_2: Keypair,
    gov_x1: XOnlyPublicKey,
    gov_x2: XOnlyPublicKey,
    utxo: (OutPoint, TxOut),
) -> anyhow::Result<String> {
    let secp = Secp256k1::new();

    let receivers_address =
        Address::from_str("bcrt1q92gkfme6k9dkpagrkwt76etkaq29hvf02w5m38f6shs4ddpw7hzqp347zm")?
            .assume_checked();

    // --- Build spending transaction ---
    let inputs: Vec<TxIn> = vec![TxIn {
        previous_output: OutPoint {
            txid: utxo.0.txid,
            vout: utxo.0.vout,
        },
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::default(),
    }];

    let outputs: Vec<TxOut> = vec![TxOut {
        value: utxo.1.value - Amount::from_sat(500),
        script_pubkey: receivers_address.script_pubkey(),
    }];

    let mut spending_tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: inputs,
        output: outputs,
    };

    // --- Control block for script path spend ---
    let control_block = spend_info
        .control_block(&(multisig_script.clone(), LeafVersion::TapScript))
        .unwrap()
        .serialize();

    // --- Sighash ---
    let mut sighash_cache = SighashCache::new(&spending_tx);
    let prevout = utxo.1.clone();
    let leaf_hash = TapLeafHash::from_script(&multisig_script, LeafVersion::TapScript);

    let sighash = sighash_cache
        .taproot_script_spend_signature_hash(
            0,
            &Prevouts::All(&[prevout.clone()]),
            leaf_hash,
            TapSighashType::All,
        )
        .expect("sighash");

    let msg = secp256k1::Message::from_digest_slice(&sighash[..]).unwrap();

    // --- Schnorr signatures from 2 of 3 governance keys ---
    let sig1 = secp.sign_schnorr(&msg, &gov_kp_1);
    let sig2 = secp.sign_schnorr(&msg, &gov_kp_2);

    let mut sig1_bytes = sig1.as_ref().to_vec();
    sig1_bytes.push(TapSighashType::All as u8);

    let mut sig2_bytes = sig2.as_ref().to_vec();
    sig2_bytes.push(TapSighashType::All as u8);

    // --- Witness stack: [sig1, sig2, tapscript, control_block] ---
    let mut witness = Witness::new();
    witness.push(&[]); // no signature for gov_x3
    witness.push(sig2_bytes);
    witness.push(sig1_bytes);
    witness.push(multisig_script.as_bytes());
    witness.push(&control_block);

    spending_tx.input[0].witness = witness;

    secp.verify_schnorr(&sig1, &msg, &gov_x1)?;
    secp.verify_schnorr(&sig2, &msg, &gov_x2)?;

    let tx_hex = bitcoin::consensus::encode::serialize_hex(&spending_tx);
    println!("tx_hex: {:?}", tx_hex);

    Ok(tx_hex)
}

pub(crate) fn receivers_address() -> Address {
    Address::from_str("bc1p0dq0tzg2r780hldthn5mrznmpxsxc0jux5f20fwj0z3wqxxk6fpqm7q0va")
        .expect("a valid address")
        .require_network(Network::Bitcoin)
        .expect("valid address for mainnet")
}
