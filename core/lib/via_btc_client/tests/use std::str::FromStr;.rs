use std::str::FromStr;

use bitcoin::hashes::Hash;
use bitcoin::key::Keypair;
use bitcoin::key::{TapTweak, TweakedKeypair, UntweakedPublicKey};
use bitcoin::locktime::absolute;
use bitcoin::opcodes::{all, OP_FALSE};
use bitcoin::script::{Builder as ScriptBuilder, PushBytesBuf};
use bitcoin::secp256k1::{rand, Message, Secp256k1, SecretKey, Signing, Verification};
use bitcoin::sighash::{EcdsaSighashType, Prevouts, SighashCache, TapSighashType};

use bitcoin::taproot::{ControlBlock, LeafVersion, TaprootBuilder};
use bitcoin::{
    transaction, Address, Amount, CompressedPublicKey, Network, OutPoint, PrivateKey, ScriptBuf,
    Sequence, TapLeafHash, Transaction, TxIn, TxOut, Txid, WPubkeyHash, Witness,
};
use bitcoincore_rpc::RawTx;

const UTXO_AMOUNT: Amount = Amount::from_sat(14_575);
const CHANGE_AMOUNT: Amount = Amount::from_sat(4_000); // 500 sat fee.

fn senders_keys<C: Signing>(secp: &Secp256k1<C>) -> (SecretKey, WPubkeyHash, Keypair) {
    let private_key_wif = "cRz3eG99BvR8VnseYPsGYEiQ8oZCgeHJxKJ3yDXPYEyNKKZHkHdB";
    let private_key = PrivateKey::from_wif(private_key_wif).expect("Invalid WIF format");
    let sk = private_key.inner;

    let pk = bitcoin::PublicKey::new(sk.public_key(secp));
    let wpkh = pk.wpubkey_hash().expect("key is compressed");

    let compressed_pk = CompressedPublicKey::from_private_key(secp, &private_key).unwrap();
    let address = Address::p2wpkh(&compressed_pk, Network::Testnet);

    println!("wpkh: {}", wpkh);
    println!("address: {}", address);

    let keypair = Keypair::from_secret_key(secp, &sk);
    (sk, wpkh, keypair)
}

fn unspent_transaction_output(wpkh: &WPubkeyHash) -> (OutPoint, TxOut) {
    let script_pubkey = ScriptBuf::new_p2wpkh(wpkh);

    let txid = "d634782c2560b95958b0831609696faadb00bacc8226f147659af8f2ebb34f8b";
    let out_point = OutPoint {
        txid: Txid::from_str(&txid).unwrap(), // Obviously invalid.
        vout: 0,
    };

    let utxo = TxOut {
        value: UTXO_AMOUNT,
        script_pubkey,
    };

    (out_point, utxo)
}

fn reveal_transaction_output_fee(wpkh: &WPubkeyHash) -> (OutPoint, TxOut) {
    let script_pubkey = ScriptBuf::new_p2wpkh(wpkh);

    let txid = "9d2d28809deff9dc156126ad9498ba43d383c0f62284c9cce7eff9c65d170b06";
    let out_point = OutPoint {
        txid: Txid::from_str(&txid).unwrap(), // Obviously invalid.
        vout: 0,
    };

    let utxo = TxOut {
        value: UTXO_AMOUNT,
        script_pubkey,
    };

    (out_point, utxo)
}

fn reveal_transaction_output_p2tr<C: Verification>(
    secp: &Secp256k1<C>,
    internal_key: UntweakedPublicKey,
) -> (OutPoint, TxOut, ScriptBuf, ControlBlock) {
    let serelized_pubkey = internal_key.serialize();
    let mut encoded_pubkey = PushBytesBuf::with_capacity(serelized_pubkey.len());
    encoded_pubkey.extend_from_slice(&serelized_pubkey).ok();

    let data: &[u8; 37] = b"***Hello From Via Inscriber: try 1***";
    println!("data: {}", data.raw_hex());
    // The inscription output with using Taproot approach:
    let taproot_script = ScriptBuilder::new()
        .push_slice(encoded_pubkey.as_push_bytes())
        .push_opcode(all::OP_CHECKSIG)
        .push_opcode(OP_FALSE)
        .push_opcode(all::OP_IF)
        .push_slice(b"***Hello From Via Inscriber: try 1***")
        .push_opcode(all::OP_ENDIF)
        .into_script();

    // Create a Taproot builder
    let mut builder = TaprootBuilder::new();
    builder = builder
        .add_leaf(0, taproot_script.clone())
        .expect("adding leaf should work");

    let taproot_spend_info = builder
        .finalize(&secp, internal_key)
        .expect("taproot finalize should work");

    let control_block = taproot_spend_info.control_block(
        &(
            taproot_script.clone(),
            LeafVersion::TapScript,
        )
    ).unwrap();
    // Create the Taproot output script
    let taproot_address = Address::p2tr_tweaked(taproot_spend_info.output_key(), Network::Testnet);
    println!("taproot address: {}", taproot_address);

    let txid = "9d2d28809deff9dc156126ad9498ba43d383c0f62284c9cce7eff9c65d170b06";
    let out_point = OutPoint {
        txid: Txid::from_str(&txid).unwrap(),
        vout: 1,
    };

    let utxo = TxOut {
        value: Amount::from_sat(0),
        script_pubkey: taproot_address.script_pubkey(),
    };

    (out_point, utxo, taproot_script, control_block)
}

#[allow(dead_code)]
pub fn process_inscribe() {
    let secp = Secp256k1::new();

    // Get a secret key we control and the pubkeyhash of the associated pubkey.
    // In a real application these would come from a stored secret.
    let (sk, wpkh, keypair) = senders_keys(&secp);
    let (internal_key, _parity) = keypair.x_only_public_key();

    // // Get an unspent output that is locked to the key above that we control.
    // // In a real application these would come from the chain.
    // let (selected_out_point, selected_utxo) = unspent_transaction_output(&wpkh);

    // // The input for the transaction we are constructing.
    // let input = TxIn {
    //     previous_output: selected_out_point, // The dummy output we are spending.
    //     script_sig: ScriptBuf::default(),    // For a p2wpkh script_sig is empty.
    //     sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
    //     witness: Witness::default(), // Filled in after signing.
    // };

    // let serelized_pubkey = internal_key.serialize();
    // let mut encoded_pubkey =
    //     PushBytesBuf::with_capacity(serelized_pubkey.len());
    // encoded_pubkey.extend_from_slice(&serelized_pubkey).ok();

    // // The inscription output with using Taproot approach:
    // let taproot_script = ScriptBuilder::new()
    //     .push_slice(encoded_pubkey.as_push_bytes())
    //     .push_opcode(all::OP_CHECKSIG)
    //     .push_opcode(OP_FALSE)
    //     .push_opcode(all::OP_IF)
    //     .push_slice(b"***Hello From Via Inscriber: try 1***")
    //     .push_opcode(all::OP_ENDIF)
    //     .into_script();

    // // Create a Taproot builder
    // let mut builder = TaprootBuilder::new();
    // builder = builder
    //     .add_leaf(0, taproot_script.clone())
    //     .expect("adding leaf should work");

    // let taproot_spend_info = builder
    //     .finalize(&secp, internal_key)
    //     .expect("taproot finalize should work");

    // // Create the Taproot output script
    // let taproot_address = Address::p2tr_tweaked(taproot_spend_info.output_key(), Network::Testnet);

    // let inscription = TxOut {
    //     value: Amount::from_sat(0),
    //     script_pubkey: taproot_address.script_pubkey(),
    // };

    // // The change output is locked to a key controlled by us.
    // let change = TxOut {
    //     value: CHANGE_AMOUNT,
    //     script_pubkey: ScriptBuf::new_p2wpkh(&wpkh), // Change comes back to us.
    // };

    // // The transaction we want to sign and broadcast.
    // let mut unsigned_commit_tx = Transaction {
    //     version: transaction::Version::TWO,  // Post BIP-68.
    //     lock_time: absolute::LockTime::ZERO, // Ignore the locktime.
    //     input: vec![input],                  // Input goes into index 0.
    //     output: vec![change, inscription],   // Outputs, order does not matter.
    // };
    // let input_index = 0;

    // // Get the sighash to sign.
    // let sighash_type = EcdsaSighashType::All;
    // let mut sighasher = SighashCache::new(&mut unsigned_commit_tx);
    // let sighash = sighasher
    //     .p2wpkh_signature_hash(
    //         input_index,
    //         &selected_utxo.script_pubkey,
    //         UTXO_AMOUNT,
    //         sighash_type,
    //     )
    //     .expect("failed to create sighash");

    // // Sign the sighash using the secp256k1 library (exported by rust-bitcoin).
    // let msg = Message::from(sighash);
    // let signature = secp.sign_ecdsa(&msg, &sk);

    // // Update the witness stack.
    // let signature = bitcoin::ecdsa::Signature {
    //     signature,
    //     sighash_type,
    // };
    // let pk = sk.public_key(&secp);
    // *sighasher.witness_mut(input_index).unwrap() = Witness::p2wpkh(&signature, &pk);

    // // Get the signed transaction.
    // let commit_tx = sighasher.into_transaction();

    // // BOOM! Transaction signed and ready to broadcast.
    // println!("{:#?}", commit_tx);

    // println!("commit transaction: {:#?}", commit_tx.raw_hex().to_string());

    //**********************************************************************************/
    // start creating reveal transaction
    //**********************************************************************************/
    // The input for the transaction we are constructing.

    let fee_input = reveal_transaction_output_fee(&wpkh);
    let reveal_input = reveal_transaction_output_p2tr(&secp, internal_key);

    let input = TxIn {
        previous_output: fee_input.0,     // The dummy output we are spending.
        script_sig: ScriptBuf::default(), // For a p2wpkh script_sig is empty.
        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
        witness: Witness::default(), // Filled in after signing.
    };

    let reveal = TxIn {
        previous_output: reveal_input.0,  // The dummy output we are spending.
        script_sig: ScriptBuf::default(), // For a p2tr script_sig is empty.
        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
        witness: Witness::default(), // Filled in after signing.
    };

    // The change output is locked to a key controlled by us.
    let change = TxOut {
        value: CHANGE_AMOUNT,
        script_pubkey: ScriptBuf::new_p2wpkh(&wpkh), // Change comes back to us.
    };

    // The transaction we want to sign and broadcast.
    let mut unsigned_reveal_tx = Transaction {
        version: transaction::Version::TWO,  // Post BIP-68.
        lock_time: absolute::LockTime::ZERO, // Ignore the locktime.
        input: vec![input, reveal],          // Input goes into index 0.
        output: vec![change],                // Outputs, order does not matter.
    };
    let fee_input_index = 0;
    let reveal_input_index = 1;

    let mut sighasher = SighashCache::new(&mut unsigned_reveal_tx);

    // **Sign the fee input**

    let sighash_type = EcdsaSighashType::All;

    let fee_input_sighash = sighasher
        .p2wpkh_signature_hash(
            fee_input_index,
            &fee_input.1.script_pubkey,
            UTXO_AMOUNT,
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
    let prevouts = [fee_input.1, reveal_input.1];
    let prevouts = Prevouts::All(&prevouts);

    let reveal_input_sighash = sighasher
        .taproot_script_spend_signature_hash(
            reveal_input_index,
            &prevouts,
            TapLeafHash::from_script(&reveal_input.2, LeafVersion::TapScript),
            sighash_type,
        )
        .expect("failed to construct sighash");

    // Sign the sighash using the secp256k1 library (exported by rust-bitcoin).
    let msg = Message::from_digest(reveal_input_sighash.to_byte_array());
    let reveal_input_signature = secp.sign_schnorr_no_aux_rand(&msg, &keypair);

    // verify
    secp.verify_schnorr(
        &reveal_input_signature,
        &msg,
        &internal_key,
    )
    .expect("signature is valid");

    // Update the witness stack.
    let reveal_input_signature = bitcoin::taproot::Signature {
        signature: reveal_input_signature,
        sighash_type,
    };

    let mut witness_data: Witness = Witness::new();

    witness_data.push(&reveal_input_signature.to_vec());
    witness_data.push(&reveal_input.2.to_bytes());

    // add control block
    witness_data.push(&reveal_input.3.serialize());

    *sighasher
        .witness_mut(reveal_input_index)
        .ok_or("failed to get witness")
        .unwrap() = witness_data;


    let reveal_tx = sighasher.into_transaction();

    // BOOM! Transaction signed and ready to broadcast.
    println!("{:#?}", reveal_tx);

    println!("reveal transaction: {:#?}", reveal_tx.raw_hex().to_string());
}