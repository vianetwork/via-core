use inquire::ui::{Color, RenderConfig, StyleSheet, Styled};
use inquire::Text;
use std::str::FromStr;
use std::vec;

use bitcoin::hashes::Hash;
use bitcoin::key::Keypair;
use bitcoin::key::UntweakedPublicKey;
use bitcoin::locktime::absolute;
use bitcoin::opcodes::{all, OP_FALSE};
use bitcoin::script::{Builder as ScriptBuilder, PushBytesBuf};
use bitcoin::secp256k1::{Message, Secp256k1, SecretKey, Signing, Verification};
use bitcoin::sighash::{EcdsaSighashType, Prevouts, SighashCache, TapSighashType};

use bitcoin::taproot::{ControlBlock, LeafVersion, TaprootBuilder, TaprootSpendInfo};
use bitcoin::{
    transaction, Address, Amount, CompressedPublicKey, Network, OutPoint, PrivateKey, ScriptBuf,
    Sequence, TapLeafHash, Transaction, TxIn, TxOut, Txid, WPubkeyHash, Witness,
};

use bitcoincore_rpc::RawTx;

use reqwest;
use serde_json::Value;

#[tokio::main]
async fn main() {
    let secp = Secp256k1::new();

    greeting();

    // get user input (private key(wif), the data to inscribe)
    let (sk, wpkh, sender_address, keypair, inscription_data) = get_user_input(&secp);
    let (internal_key, _parity) = keypair.x_only_public_key();

    println!("calling api to fetch all utxos for the given address...");
    let utxos = get_utxos(&sender_address).await;

    let (commit_tx_inputs, unlocked_value, inputs_count, script_pubkeys, utxo_amounts) =
        constructing_commit_tx_input(utxos);

    let (inscription_script, inscription_script_size) =
        get_insription_script(&inscription_data, internal_key);

    let (inscription_commitment_output, taproot_spend_info) =
        construct_inscription_commitment_output(&secp, inscription_script.clone(), internal_key);

    let estimated_commitment_tx_size = estimate_transaction_size(inputs_count, 0, 1, 1, vec![]);

    let fee_rate = get_fee_rate().await;

    let estimated_fee = u64::from(fee_rate) * estimated_commitment_tx_size as u64;
    let estimated_fee = Amount::from_sat(estimated_fee);

    println!("fee rate: {:?}", fee_rate);
    println!(
        "Estimated commitment tx size: {:?}",
        estimated_commitment_tx_size
    );
    println!("Estimated fee: {:?}", estimated_fee);

    let commit_change_value = unlocked_value - estimated_fee;

    let change_output = TxOut {
        value: commit_change_value,
        script_pubkey: ScriptBuf::new_p2wpkh(&wpkh),
    };

    let mut unsigned_commit_tx = Transaction {
        version: transaction::Version::TWO,  // Post BIP-68.
        lock_time: absolute::LockTime::ZERO, // Ignore the locktime.
        input: commit_tx_inputs.clone(),     // Input goes into index 0.
        output: vec![change_output, inscription_commitment_output], // Outputs, order does not matter.
    };

    let sighash_type = EcdsaSighashType::All;
    let mut commit_tx_sighasher = SighashCache::new(&mut unsigned_commit_tx);

    for (index, _input) in commit_tx_inputs.iter().enumerate() {
        let sighash = commit_tx_sighasher
            .p2wpkh_signature_hash(
                index,
                &script_pubkeys[index],
                utxo_amounts[index],
                sighash_type,
            )
            .expect("failed to create sighash");

        // Sign the sighash using the secp256k1 library (exported by rust-bitcoin).
        let msg = Message::from(sighash);
        let signature = secp.sign_ecdsa(&msg, &sk);

        // Update the witness stack.
        let signature = bitcoin::ecdsa::Signature {
            signature,
            sighash_type,
        };
        let pk = sk.public_key(&secp);
        *commit_tx_sighasher.witness_mut(index).unwrap() = Witness::p2wpkh(&signature, &pk);
    }

    // Get the signed transaction.
    let commit_tx = commit_tx_sighasher.into_transaction();

    // BOOM! Transaction signed and ready to broadcast.
    println!("{:#?}", commit_tx);

    println!("commit transaction: {:#?}", commit_tx.raw_hex().to_string());

    let txid = commit_tx.compute_wtxid();
    println!("commit txid: {:?}", txid);

    let txid = commit_tx.compute_txid();
    println!("commit txid: {:?}", txid);

    // START CREATING REVEAL TRANSACTION

    let fee_payer_utxo =
        reveal_transaction_output_fee(&wpkh, &txid.to_string(), commit_change_value);

    let reveal_input =
        reveal_transaction_output_p2tr(&inscription_script, &txid.to_string(), taproot_spend_info);

    let input = TxIn {
        previous_output: fee_payer_utxo.0, // The dummy output we are spending.
        script_sig: ScriptBuf::default(),  // For a p2wpkh script_sig is empty.
        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
        witness: Witness::default(), // Filled in after signing.
    };

    let reveal = TxIn {
        previous_output: reveal_input.0,  // The dummy output we are spending.
        script_sig: ScriptBuf::default(), // For a p2tr script_sig is empty.
        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
        witness: Witness::default(), // Filled in after signing.
    };

    let reveal_tx_estimate_size =
        estimate_transaction_size(1, 1, 1, 0, vec![inscription_script_size]);

    let reveal_fee = u64::from(fee_rate) * reveal_tx_estimate_size as u64;
    let reveal_fee = Amount::from_sat(reveal_fee);

    println!("Estimated reveal tx size: {:?}", reveal_tx_estimate_size);
    println!("reveal fee: {:?}", reveal_fee);

    let reveal_change_value = fee_payer_utxo.1.value - reveal_fee;

    let reveal_change_output = TxOut {
        value: reveal_change_value,
        script_pubkey: ScriptBuf::new_p2wpkh(&wpkh),
    };

    // The transaction we want to sign and broadcast.
    let mut unsigned_reveal_tx = Transaction {
        version: transaction::Version::TWO,  // Post BIP-68.
        lock_time: absolute::LockTime::ZERO, // Ignore the locktime.
        input: vec![input, reveal],          // Input goes into index 0.
        output: vec![reveal_change_output],  // Outputs, order does not matter.
    };
    let fee_input_index = 0;
    let reveal_input_index = 1;

    let mut sighasher = SighashCache::new(&mut unsigned_reveal_tx);

    let sighash_type = EcdsaSighashType::All;

    let fee_input_sighash = sighasher
        .p2wpkh_signature_hash(
            fee_input_index,
            &fee_payer_utxo.1.script_pubkey,
            fee_payer_utxo.1.value,
            sighash_type,
        )
        .expect("failed to create sighash");

    // Sign the sighash using the secp256k1 library (exported by rust-bitcoin).
    let msg = Message::from(fee_input_sighash);
    let fee_input_signature = secp.sign_ecdsa(&msg, &sk);

    // Update the witness stack.
    let fee_input_signature = bitcoin::ecdsa::Signature {
        signature: fee_input_signature,
        sighash_type,
    };
    let pk = sk.public_key(&secp);

    *sighasher.witness_mut(fee_input_index).unwrap() = Witness::p2wpkh(&fee_input_signature, &pk);

    // **Sign the reveal input**

    let sighash_type = TapSighashType::All;
    let prevouts = [fee_payer_utxo.1, reveal_input.1];
    let prevouts = Prevouts::All(&prevouts);

    let reveal_input_sighash = sighasher
        .taproot_script_spend_signature_hash(
            reveal_input_index,
            &prevouts,
            TapLeafHash::from_script(&inscription_script, LeafVersion::TapScript),
            sighash_type,
        )
        .expect("failed to construct sighash");

    // Sign the sighash using the secp256k1 library (exported by rust-bitcoin).
    let msg = Message::from_digest(reveal_input_sighash.to_byte_array());
    let reveal_input_signature = secp.sign_schnorr_no_aux_rand(&msg, &keypair);

    // verify
    secp.verify_schnorr(&reveal_input_signature, &msg, &internal_key)
        .expect("signature is valid");

    // Update the witness stack.
    let reveal_input_signature = bitcoin::taproot::Signature {
        signature: reveal_input_signature,
        sighash_type,
    };

    let mut witness_data: Witness = Witness::new();

    witness_data.push(&reveal_input_signature.to_vec());
    witness_data.push(&inscription_script.to_bytes());

    // add control block
    witness_data.push(&reveal_input.2.serialize());

    *sighasher
        .witness_mut(reveal_input_index)
        .ok_or("failed to get witness")
        .unwrap() = witness_data;

    let reveal_tx = sighasher.into_transaction();

    // BOOM! Transaction signed and ready to broadcast.
    println!("{:#?}", reveal_tx);

    println!("reveal transaction: {:#?}", reveal_tx.raw_hex().to_string());

    let txid = reveal_tx.compute_wtxid();
    println!("reveal txid: {:?}", txid);

    let txid = reveal_tx.compute_txid();
    println!("reveal txid: {:?}", txid);
}

fn reveal_transaction_output_p2tr(
    inscription_script: &ScriptBuf,
    txid: &str,
    taproot_spend_info: TaprootSpendInfo,
) -> (OutPoint, TxOut, ControlBlock) {
    let control_block = taproot_spend_info
        .control_block(&(inscription_script.clone(), LeafVersion::TapScript))
        .unwrap();

    let out_point = OutPoint {
        txid: Txid::from_str(&txid).unwrap(),
        vout: 1,
    };

    let taproot_address = Address::p2tr_tweaked(taproot_spend_info.output_key(), Network::Testnet);

    let utxo = TxOut {
        value: Amount::from_sat(0),
        script_pubkey: taproot_address.script_pubkey(),
    };

    (out_point, utxo, control_block)
}

fn reveal_transaction_output_fee(
    wpkh: &WPubkeyHash,
    txid: &str,
    change_amount: Amount,
) -> (OutPoint, TxOut) {
    let script_pubkey = ScriptBuf::new_p2wpkh(wpkh);

    let out_point = OutPoint {
        txid: Txid::from_str(&txid).unwrap(), // Obviously invalid.
        vout: 0,
    };

    let utxo = TxOut {
        value: change_amount,
        script_pubkey,
    };

    (out_point, utxo)
}
fn estimate_transaction_size(
    p2wpkh_inputs_count: u32,
    p2tr_inputs_count: u32,
    p2wpkh_outputs_count: u32,
    p2tr_outputs_count: u32,
    p2tr_witness_sizes: Vec<usize>,
) -> usize {
    // https://bitcoinops.org/en/tools/calc-size/
    // https://en.bitcoin.it/wiki/Protocol_documentation#Common_structures
    // https://btcinformation.org/en/developer-reference#p2p-network

    assert!(p2tr_inputs_count == p2tr_witness_sizes.len() as u32);

    let version_size = 4;
    let input_count_size = 1;
    let output_count_size = 1;
    let locktime_size = 4;
    let maker_flags_size = 1; // 1/2

    let base_size =
        version_size + input_count_size + output_count_size + locktime_size + maker_flags_size;

    // p2wpkh input base size
    // out point (36) The txid and vout index number of the output (UTXO) being spent
    // scriptSig length  (1)
    // scriptSig (0) for p2wpkh and p2tr the scriptSig is empty
    // sequence number (4)
    // Witness item count (1/4)
    // witness item (27)
    //     ( (73) size signature + (34) size public_key ) / 4
    // 36 + 1 + 0 + 4 + 1 + 27 = 69
    let p2wpkh_input_base_size = 69;

    // p2tr input base size
    // out point (36) The txid and vout index number of the output (UTXO) being spent
    // scriptSig length  (1)
    // scriptSig (0) for p2wpkh and p2tr the scriptSig is empty
    // sequence number (4)
    // Witness item count (3)
    // witness item (17)
    //     ( 65) size schnorr_signature / 4
    // * rest of the witness items size is calculated based on the witness size
    // 36 + 1 + 0 + 4 + 3 + 17 = 61
    let p2tr_input_base_size = 61;

    // p2wpkh output base size
    // value (8)
    // scriptPubKey length (1)
    // scriptPubKey (p2wpkh: 25)
    // 8 + 1 + 25 = 34
    let p2wpkh_output_base_size = 34;

    // p2tr output base size
    // value (8)
    // scriptPubKey length (1)
    // scriptPubKey (p2tr: 34)
    // 8 + 1 + 34 = 43
    let p2tr_output_base_size = 43;

    let p2wpkh_input_size = p2wpkh_input_base_size * p2wpkh_inputs_count as usize;

    let mut p2tr_input_size = 0;

    for p2tr_witness_size in p2tr_witness_sizes {
        p2tr_input_size += p2tr_input_base_size + p2tr_witness_size;
    }

    let p2wpkh_output_size = p2wpkh_output_base_size * p2wpkh_outputs_count as usize;
    let p2tr_output_size = p2tr_output_base_size * p2tr_outputs_count as usize;

    let total_size =
        base_size + p2wpkh_input_size + p2tr_input_size + p2wpkh_output_size + p2tr_output_size;

    return total_size;
}

async fn get_fee_rate() -> u64 {
    // https://mempool.space/testnet/api/v1/fees/recommended
    let url = "https://mempool.space/testnet/api/v1/fees/recommended";
    let res = reqwest::get(url).await.unwrap();
    let res = res.text().await.unwrap();

    let res_json: Value = serde_json::from_str(&res).unwrap();

    let fastest_fee_rate = res_json.get("fastestFee").unwrap().as_u64().unwrap();

    return fastest_fee_rate;
}

fn construct_inscription_commitment_output<C: Signing + Verification>(
    secp: &Secp256k1<C>,
    inscription_script: ScriptBuf,
    internal_key: UntweakedPublicKey,
) -> (TxOut, TaprootSpendInfo) {
    // Create a Taproot builder
    let mut builder = TaprootBuilder::new();
    builder = builder
        .add_leaf(0, inscription_script.clone())
        .expect("adding leaf should work");

    let taproot_spend_info = builder
        .finalize(&secp, internal_key)
        .expect("taproot finalize should work");

    // Create the Taproot output script
    let taproot_address = Address::p2tr_tweaked(taproot_spend_info.output_key(), Network::Testnet);

    let inscription = TxOut {
        value: Amount::from_sat(0),
        script_pubkey: taproot_address.script_pubkey(),
    };

    return (inscription, taproot_spend_info);
}

fn get_insription_script(
    inscription_data: &str,
    internal_key: UntweakedPublicKey,
) -> (ScriptBuf, usize) {
    let serelized_pubkey = internal_key.serialize();
    let mut encoded_pubkey = PushBytesBuf::with_capacity(serelized_pubkey.len());
    encoded_pubkey.extend_from_slice(&serelized_pubkey).ok();

    let data = inscription_data.as_bytes();
    let mut encoded_data = PushBytesBuf::with_capacity(data.len());
    encoded_data.extend_from_slice(data).ok();

    let taproot_script = ScriptBuilder::new()
        .push_slice(encoded_pubkey.as_push_bytes())
        .push_opcode(all::OP_CHECKSIG)
        .push_opcode(OP_FALSE)
        .push_opcode(all::OP_IF)
        .push_slice(encoded_data)
        .push_opcode(all::OP_ENDIF)
        .into_script();

    let script_bytes_size = taproot_script.len();

    return (taproot_script, script_bytes_size);
}

fn constructing_commit_tx_input(
    utxos: Vec<(OutPoint, TxOut)>,
) -> (Vec<TxIn>, Amount, u32, Vec<ScriptBuf>, Vec<Amount>) {
    let mut txins: Vec<TxIn> = vec![];
    let mut total_value = Amount::ZERO;
    let mut num_inputs = 0;
    let mut script_pubkeys: Vec<ScriptBuf> = vec![];
    let mut amounts: Vec<Amount> = vec![];

    for (outpoint, txout) in utxos {
        let txin = TxIn {
            previous_output: outpoint,
            script_sig: ScriptBuf::default(), // For a p2wpkh script_sig is empty.
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::default(), // Get filled in after signing.
        };

        txins.push(txin);
        total_value += txout.value;
        num_inputs += 1;
        script_pubkeys.push(txout.script_pubkey);
        amounts.push(txout.value);
    }
    (txins, total_value, num_inputs, script_pubkeys, amounts)
}

async fn get_utxos(addr: &Address) -> Vec<(OutPoint, TxOut)> {
    // call blockcypher api to get all utxos for the given address
    // https://api.blockcypher.com/v1/btc/test3/addrs/tb1qvxglm3jqsawtct65drunhe6uvat2k58dhfugqu/full?limit=200

    let url = format!(
        "https://api.blockcypher.com/v1/btc/test3/addrs/{}/full?limit=200",
        addr
    );
    let res = reqwest::get(&url).await.unwrap().text().await.unwrap();

    // Convert the response string to JSON
    let res_json: Value = serde_json::from_str(&res).unwrap();

    let balance = res_json.get("final_balance").unwrap().as_u64().unwrap();

    println!("your address balance is {:?} sats", balance);

    let txs = res_json.get("txs").unwrap().as_array().unwrap();

    println!("found {} transactions", txs.len());

    let mut utxos: Vec<(OutPoint, TxOut)> = vec![];

    for tx in txs {
        let txid = tx.get("hash").unwrap().as_str().unwrap();
        let txid = Txid::from_str(txid).unwrap();

        let vouts = tx.get("outputs").unwrap().as_array().unwrap();

        for (vout_index, vout) in vouts.iter().enumerate() {
            let mut is_valid = true;
            let value = vout.get("value").unwrap().as_u64().unwrap();

            if vout.get("spent_by").is_some() {
                is_valid = false;
            }

            if vout.get("script_type").unwrap().as_str().unwrap() != "pay-to-witness-pubkey-hash" {
                println!(
                    "skipping non-p2wpkh output ... {:?}",
                    vout.get("script_type").unwrap().as_str().unwrap()
                );

                is_valid = false;
            }

            if value == 0 {
                println!("skipping zero value output ...");
                is_valid = false;
            }

            if !is_valid {
                continue;
            }

            let out_point = OutPoint {
                txid,
                vout: vout_index as u32,
            };

            let tx_out = TxOut {
                value: Amount::from_sat(value),
                script_pubkey: ScriptBuf::from_hex(vout.get("script").unwrap().as_str().unwrap())
                    .unwrap(),
            };

            utxos.push((out_point, tx_out));
            println!("found utxo: {:?}", txid);
        }
    }

    return utxos;
}

fn get_user_input<C: Signing>(
    secp: &Secp256k1<C>,
) -> (SecretKey, WPubkeyHash, Address, Keypair, String) {
    let mut render_config = RenderConfig::default();
    render_config.prompt_prefix = Styled::new(">").with_fg(Color::LightGreen);
    render_config.prompt = StyleSheet::new().with_fg(Color::LightMagenta);

    let user_wif_prv = Text::new("Enter your private key (WIF): ")
        .with_render_config(render_config)
        .prompt()
        .unwrap();

    let user_wif_prv = user_wif_prv.trim();

    let private_key = PrivateKey::from_wif(user_wif_prv).expect("Invalid Private Key WIF format");
    let sk = private_key.inner;

    let pk = bitcoin::PublicKey::new(sk.public_key(secp));
    let wpkh = pk.wpubkey_hash().expect("key is compressed");

    let compressed_pk = CompressedPublicKey::from_private_key(secp, &private_key).unwrap();
    let address = Address::p2wpkh(&compressed_pk, Network::Testnet);

    let keypair = Keypair::from_secret_key(secp, &sk);

    println!("Your address: {}", address);

    let multiline_content = r#"
    Please check the printed address above and make sure it is correct.
    if it's not press ctrl+c to exit and try again.

    Enter the data you want to inscribe (string or hexstring): 
    "#;
    let data = Text::new(multiline_content)
        .with_render_config(render_config)
        .prompt()
        .unwrap();

    let trimmed_data = data.trim().to_string();

    (sk, wpkh, address, keypair, trimmed_data)
}

fn greeting() {
    let content = r#"
    
    Welcome! 
    
    This is an CLI application that walks you through 
    inscribing arbitrary data into the Bitcoin testnet.

    **Please before continuing make sure you have done the following:**
    
    1- Install electrum wallet (https://electrum.org/#download)
    And run it in testnet mode with using the following command:
    Linux: electrum --testnet
    Mac: /Applications/Electrum.app/Contents/MacOS/run_electrum --testnet

    2- create a p2wpkh wallet (this is the default wallet type in electrum).
    
    3- get some testnet coins.
    
    Faucet Links:
        https://bitcoinfaucet.uo1.net/
        https://coinfaucet.eu/en/btc-testnet/
    
    when you are ready, press enter to continue...
    "#;

    let mut render_config = RenderConfig::default();
    render_config.prompt_prefix =
        Styled::new("***********************************************************")
            .with_fg(Color::LightRed);
    render_config.prompt = StyleSheet::new().with_fg(Color::Grey);

    let res = Text::new(content)
        .with_render_config(render_config)
        .prompt();

    match res {
        Ok(_) => {}
        Err(e) => println!("Error: {}", e),
    }
}
