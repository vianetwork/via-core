use std::{str::FromStr, vec};

use anyhow::{Context, Result};
use bitcoin::{
    hashes::Hash,
    key::{Keypair, UntweakedPublicKey},
    locktime::absolute,
    opcodes::{all, OP_FALSE},
    script::{Builder as ScriptBuilder, PushBytesBuf},
    secp256k1::{All, Message, Secp256k1, SecretKey, Signing, Verification},
    sighash::{EcdsaSighashType, Prevouts, SighashCache, TapSighashType},
    taproot::{ControlBlock, LeafVersion, TaprootBuilder, TaprootSpendInfo},
    transaction, Address, Amount, CompressedPublicKey, Network, OutPoint, PrivateKey, ScriptBuf,
    Sequence, TapLeafHash, Transaction, TxIn, TxOut, Txid, WPubkeyHash, Witness,
};
use bitcoincore_rpc::RawTx;
use inquire::{
    ui::{Color, RenderConfig, StyleSheet},
    Text,
};
use serde_json::Value;

const MAX_PUSH_SIZE: usize = 520;

// Fee Estimation Constants
const VERSION_SIZE: usize = 4;
const INPUT_COUNT_SIZE: usize = 1;
const OUTPUT_COUNT_SIZE: usize = 1;
const LOCKTIME_SIZE: usize = 4;
const MAKER_FLAGS_SIZE: usize = 1; // 1/2

// p2wpkh input base size
// out point (36) The txid and vout index number of the output (UTXO) being spent
// scriptSig length  (1)
// scriptSig (0) for p2wpkh and p2tr the scriptSig is empty
// sequence number (4)
// Witness item count (1/4)
// witness item (27)
//     ( (73) size signature + (34) size public_key ) / 4
// 36 + 1 + 0 + 4 + 1 + 27 = 69
const P2WPKH_INPUT_BASE_SIZE: usize = 69;

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
const P2TR_INPUT_BASE_SIZE: usize = 61;

// p2wpkh output base size
// value (8)
// scriptPubKey length (1)
// scriptPubKey (p2wpkh: 25)
// 8 + 1 + 25 = 34
const P2WPKH_OUTPUT_BASE_SIZE: usize = 34;

// p2tr output base size
// value (8)
// scriptPubKey length (1)
// scriptPubKey (p2tr: 34)
// 8 + 1 + 34 = 43
const P2TR_OUTPUT_BASE_SIZE: usize = 43;

struct UserKey {
    sk: SecretKey,
    wpkh: WPubkeyHash,
    address: Address,
    keypair: Keypair,
    internal_key: UntweakedPublicKey,
}

impl UserKey {
    pub fn new<C: Signing>(
        wif_string: &str,
        secp: &Secp256k1<C>,
        network: Network,
    ) -> Result<Self> {
        let private_key =
            PrivateKey::from_wif(wif_string).context("Invalid Private Key WIF format")?;
        let sk = private_key.inner;

        let pk = bitcoin::PublicKey::new(sk.public_key(secp));
        let wpkh = pk.wpubkey_hash().context("key is compressed")?;

        let compressed_pk = CompressedPublicKey::from_private_key(secp, &private_key)
            .context("Failed to get compressed public key from private key")?;
        let address = Address::p2wpkh(&compressed_pk, network);

        let keypair = Keypair::from_secret_key(secp, &sk);

        let internal_key = keypair.x_only_public_key().0;

        let res = UserKey {
            sk,
            wpkh,
            address,
            keypair,
            internal_key,
        };

        Ok(res)
    }
}

struct InscriptionData {
    inscription_script: ScriptBuf,
    script_size: usize,
    script_pubkey: ScriptBuf,
    taproot_spend_info: TaprootSpendInfo,
}

impl InscriptionData {
    pub fn new<C: Signing + Verification>(
        secp: &Secp256k1<C>,
        data: String,
        internal_key: UntweakedPublicKey,
        network: Network,
    ) -> Result<Self> {
        let serelized_pubkey = internal_key.serialize();
        let mut encoded_pubkey = PushBytesBuf::with_capacity(serelized_pubkey.len());
        encoded_pubkey.extend_from_slice(&serelized_pubkey).ok();

        let is_long = data.as_bytes().len() > MAX_PUSH_SIZE;

        let (inscription_script, script_size) = if is_long {
            Self::construct_big_script(&data, &encoded_pubkey)
        } else {
            Self::construct_normal_script(&data, &encoded_pubkey)
        };

        let (inscription_script_pub, taproot_spend_info) =
            Self::construct_inscription_commitment_output(
                secp,
                &inscription_script,
                internal_key,
                network,
            )?;

        let res = InscriptionData {
            inscription_script,
            script_size,
            script_pubkey: inscription_script_pub,
            taproot_spend_info,
        };

        Ok(res)
    }

    fn construct_inscription_commitment_output<C: Signing + Verification>(
        secp: &Secp256k1<C>,
        inscription_script: &ScriptBuf,
        internal_key: UntweakedPublicKey,
        network: Network,
    ) -> Result<(ScriptBuf, TaprootSpendInfo)> {
        let mut builder = TaprootBuilder::new();
        builder = builder
            .add_leaf(0, inscription_script.clone())
            .context("adding leaf should work")?;

        let taproot_spend_info = builder
            .finalize(secp, internal_key)
            .map_err(|e| anyhow::anyhow!("Failed to finalize taproot spend info: {:?}", e))?;

        let taproot_address = Address::p2tr_tweaked(taproot_spend_info.output_key(), network);

        let script_pubkey = taproot_address.script_pubkey();

        Ok((script_pubkey, taproot_spend_info))
    }

    fn construct_normal_script(data: &str, encoded_pubkey: &PushBytesBuf) -> (ScriptBuf, usize) {
        let data = data.as_bytes();
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

        (taproot_script, script_bytes_size)
    }

    fn construct_big_script(data: &str, encoded_pubkey: &PushBytesBuf) -> (ScriptBuf, usize) {
        let data = data.as_bytes();

        let mut chunks: Vec<PushBytesBuf> = vec![];

        let chunks_len = data.len() / MAX_PUSH_SIZE;

        for i in 0..chunks_len {
            let start = i * MAX_PUSH_SIZE;
            let end = (i + 1) * MAX_PUSH_SIZE;
            let mut encoded_data = PushBytesBuf::with_capacity(MAX_PUSH_SIZE);
            encoded_data.extend_from_slice(&data[start..end]).ok();
            chunks.push(encoded_data);
        }

        let last_chunk = data.len() % MAX_PUSH_SIZE;

        let mut encoded_data = PushBytesBuf::with_capacity(last_chunk);
        encoded_data
            .extend_from_slice(&data[chunks_len * MAX_PUSH_SIZE..])
            .ok();
        chunks.push(encoded_data);

        let mut script = ScriptBuilder::new()
            .push_slice(encoded_pubkey.as_push_bytes())
            .push_opcode(all::OP_CHECKSIG)
            .push_opcode(OP_FALSE)
            .push_opcode(all::OP_IF);

        for chunk in chunks {
            script = script.push_slice(chunk);
        }

        let tap_script = script.push_opcode(all::OP_ENDIF).into_script();

        let script_bytes_size = tap_script.len();

        (tap_script, script_bytes_size)
    }
}
struct InscriptionManager {
    network: Network,
    secp: Secp256k1<All>,
    utxo_api_url: String,
    fee_rate_api_url: String,
    user_key: Option<UserKey>,
    inscription_data: Option<InscriptionData>,
}

struct InscriptionManagerResponse {
    commit_raw_tx: String,
    commit_txid: String,
    commit_fee: Amount,
    commit_tx_size: usize,
    commit_change_value: Amount,
    commit_input_count: u32,
    commit_output_count: u32,
    reveal_raw_tx: String,
    reveal_txid: String,
    reveal_fee: Amount,
    reveal_tx_size: usize,
    reveal_change_value: Amount,
    reveal_input_count: u32,
    reveal_output_count: u32,
}
struct CommitTxInputRes {
    commit_tx_inputs: Vec<TxIn>,
    unlocked_value: Amount,
    inputs_count: u32,
    script_pubkeys: Vec<ScriptBuf>,
    utxo_amounts: Vec<Amount>,
}

impl InscriptionManager {
    pub fn new(network: Network, utxo_api_url: String, fee_rate_api_url: String) -> Self {
        let secp = Secp256k1::new();
        InscriptionManager {
            network,
            secp,
            utxo_api_url,
            fee_rate_api_url,
            user_key: None,
            inscription_data: None,
        }
    }

    pub fn proccess_user_input(&mut self, wif_string: &str, inscription_data: &str) -> Result<()> {
        let user_key = UserKey::new(wif_string, &self.secp, self.network);
        self.user_key = Some(user_key?);

        let internal_key = self
            .user_key
            .as_ref()
            .context("User key is not set")?
            .internal_key;

        let inscription_data = InscriptionData::new(
            &self.secp,
            inscription_data.to_string(),
            internal_key,
            self.network,
        );

        self.inscription_data = Some(inscription_data?);

        Ok(())
    }

    async fn get_utxos(&self) -> Result<Vec<(OutPoint, TxOut)>> {
        // call blockcypher api to get all utxos for the given address
        // https://api.blockcypher.com/v1/btc/test3/addrs/tb1qvxglm3jqsawtct65drunhe6uvat2k58dhfugqu/full?limit=200

        let address = self
            .user_key
            .as_ref()
            .context("User key is not set")?
            .address
            .clone()
            .to_string();

        // "https://api.blockcypher.com/v1/btc/test3/addrs/{}/full?limit=200"
        let url = format!("{}/{}/full?limit=200", self.utxo_api_url, address);

        let res = reqwest::get(url)
            .await
            .context("Failed to get utxos from api")?
            .text()
            .await
            .context("Failed to get utxos from api")?;

        // Convert the response string to JSON
        let res_json: Value = serde_json::from_str(&res).context("Failed to parse response")?;

        let balance = res_json
            .get("final_balance")
            .context("Failed to get balance from response")?
            .as_u64()
            .context("Failed to parse balance")?;

        println!("your address balance is {:?} sats", balance);

        let txs = res_json
            .get("txs")
            .context("Failed to get transactions from response")?
            .as_array()
            .context("Failed to parse transactions")?;

        println!("found {} transactions", txs.len());

        let mut utxos: Vec<(OutPoint, TxOut)> = vec![];

        for tx in txs {
            let txid = tx
                .get("hash")
                .context("Failed to get txid from transaction")?
                .as_str()
                .context("Failed to parse txid")?;
            let txid = Txid::from_str(txid).context("Failed to parse txid")?;

            let vouts = tx
                .get("outputs")
                .context("Failed to get outputs from transaction")?
                .as_array()
                .context("Failed to parse outputs")?;

            let confirmations = tx
                .get("confirmations")
                .context("Failed to get confirmations from transaction")?
                .as_u64()
                .context("Failed to parse confirmations")?;

            if confirmations == 0 {
                println!("skipping unconfirmed transaction ...");
                continue;
            }

            for (vout_index, vout) in vouts.iter().enumerate() {
                let mut is_valid = true;
                let value = vout
                    .get("value")
                    .context("Failed to get value from output")?
                    .as_u64()
                    .context("Failed to parse value")?;

                if vout.get("spent_by").is_some() {
                    is_valid = false;
                }

                if vout
                    .get("script_type")
                    .context("Failed to get script type")?
                    .as_str()
                    != Some("pay-to-witness-pubkey-hash")
                {
                    let script_type = vout
                        .get("script_type")
                        .context("Failed to get script type")?
                        .as_str()
                        .context("Failed to parse script type")?;
                    println!("skipping non-p2wpkh output ... {:?}", script_type);

                    is_valid = false;
                }

                let vout_related_addresses = vout
                    .get("addresses")
                    .context("Failed to get addresses from output")?
                    .as_array()
                    .context("Failed to parse addresses")?;

                for vout_address in vout_related_addresses {
                    let vout_address = vout_address.as_str().context("Failed to parse address")?;

                    if vout_address != address {
                        println!("skipping unrelated address output ...");
                        is_valid = false;
                    }
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
                    script_pubkey: ScriptBuf::from_hex(
                        vout.get("script")
                            .context("Failed to get script from output")?
                            .as_str()
                            .context("Failed to parse script")?,
                    )
                    .context("Failed to parse script")?,
                };

                utxos.push((out_point, tx_out));
                println!("found utxo: {:?}", txid);
            }
        }

        Ok(utxos)
    }

    fn constructing_commit_tx_input(
        &self,
        utxos: Vec<(OutPoint, TxOut)>,
    ) -> Result<CommitTxInputRes> {
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

        let res = CommitTxInputRes {
            commit_tx_inputs: txins,
            unlocked_value: total_value,
            inputs_count: num_inputs,
            script_pubkeys,
            utxo_amounts: amounts,
        };

        Ok(res)
    }

    fn estimate_transaction_size(
        &self,
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

        let base_size =
            VERSION_SIZE + INPUT_COUNT_SIZE + OUTPUT_COUNT_SIZE + LOCKTIME_SIZE + MAKER_FLAGS_SIZE;

        let p2wpkh_input_size = P2WPKH_INPUT_BASE_SIZE * p2wpkh_inputs_count as usize;

        let mut p2tr_input_size = 0;

        for witness_size in p2tr_witness_sizes {
            p2tr_input_size += P2TR_INPUT_BASE_SIZE + witness_size;
        }

        let p2wpkh_output_size = P2WPKH_OUTPUT_BASE_SIZE * p2wpkh_outputs_count as usize;

        let p2tr_output_size = P2TR_OUTPUT_BASE_SIZE * p2tr_outputs_count as usize;

        base_size + p2wpkh_input_size + p2tr_input_size + p2wpkh_output_size + p2tr_output_size
    }

    async fn get_fee_rate(&self) -> Result<u64> {
        // https://mempool.space/testnet/api/v1/fees/recommended

        let res = reqwest::get(&self.fee_rate_api_url)
            .await
            .context("Failed to get fee rate")?;
        let res = res
            .text()
            .await
            .context("Failed to get fee rate from api")?;

        let res_json: Value =
            serde_json::from_str(&res).context("Failed to parse fee rate response")?;

        let fastest_fee_rate = res_json
            .get("fastestFee")
            .context("Failed to get fastest fee rate")?
            .as_u64()
            .context("Failed to parse fastest fee rate")?;

        Ok(fastest_fee_rate)
    }

    fn reveal_transaction_normal_input(
        &self,
        wpkh: &WPubkeyHash,
        txid: Txid,
        change_amount: Amount,
    ) -> Result<(OutPoint, TxOut)> {
        let script_pubkey = ScriptBuf::new_p2wpkh(wpkh);

        let out_point = OutPoint { txid, vout: 0 };

        let utxo = TxOut {
            value: change_amount,
            script_pubkey,
        };

        Ok((out_point, utxo))
    }

    fn reveal_transaction_p2tr_input(
        &self,
        inscription_script: &ScriptBuf,
        txid: Txid,
        taproot_spend_info: TaprootSpendInfo,
    ) -> Result<(OutPoint, TxOut, ControlBlock)> {
        let control_block = taproot_spend_info
            .control_block(&(inscription_script.clone(), LeafVersion::TapScript))
            .ok_or_else(|| anyhow::anyhow!("Failed to get control block"))?;

        let out_point = OutPoint { txid, vout: 1 };

        let taproot_address = Address::p2tr_tweaked(taproot_spend_info.output_key(), self.network);

        let utxo = TxOut {
            value: Amount::from_sat(0),
            script_pubkey: taproot_address.script_pubkey(),
        };

        Ok((out_point, utxo, control_block))
    }

    pub async fn start(&self) -> Result<InscriptionManagerResponse> {
        let utxos = self.get_utxos().await?;

        let commit_tx_input_info = self.constructing_commit_tx_input(utxos)?;

        let inscription_commitment_output: TxOut = TxOut {
            value: Amount::ZERO,
            script_pubkey: self
                .inscription_data
                .as_ref()
                .context("Inscription data is not set")?
                .script_pubkey
                .clone(),
        };

        let estimated_commitment_tx_size =
            self.estimate_transaction_size(commit_tx_input_info.inputs_count, 0, 1, 1, vec![]);
        let fee_rate = self.get_fee_rate().await?;

        let estimated_fee = fee_rate * estimated_commitment_tx_size as u64;
        let commit_estimated_fee = Amount::from_sat(estimated_fee);

        let commit_change_value = commit_tx_input_info.unlocked_value - commit_estimated_fee;

        let change_output = TxOut {
            value: commit_change_value,
            script_pubkey: ScriptBuf::new_p2wpkh(
                &self.user_key.as_ref().context("User key is not set")?.wpkh,
            ),
        };

        let mut unsigned_commit_tx = Transaction {
            version: transaction::Version::TWO,  // Post BIP-68.
            lock_time: absolute::LockTime::ZERO, // Ignore the locktime.
            input: commit_tx_input_info.commit_tx_inputs.clone(), // Input goes into index 0.
            output: vec![change_output, inscription_commitment_output], // Outputs, order does not matter.
        };

        let sighash_type = EcdsaSighashType::All;
        let mut commit_tx_sighasher = SighashCache::new(&mut unsigned_commit_tx);

        for (index, _input) in commit_tx_input_info.commit_tx_inputs.iter().enumerate() {
            let sighash = commit_tx_sighasher
                .p2wpkh_signature_hash(
                    index,
                    &commit_tx_input_info.script_pubkeys[index],
                    commit_tx_input_info.utxo_amounts[index],
                    sighash_type,
                )
                .context("Failed to create sighash")?;

            // Sign the sighash using the secp256k1 library (exported by rust-bitcoin).
            let msg = Message::from(sighash);
            let signature = self.secp.sign_ecdsa(
                &msg,
                &self.user_key.as_ref().context("User key is not set")?.sk,
            );

            // Update the witness stack.
            let signature = bitcoin::ecdsa::Signature {
                signature,
                sighash_type,
            };
            let pk = self
                .user_key
                .as_ref()
                .context("User key is not set")?
                .sk
                .public_key(&self.secp);
            *commit_tx_sighasher
                .witness_mut(index)
                .ok_or_else(|| anyhow::anyhow!("Failed to get witness"))? =
                Witness::p2wpkh(&signature, &pk);
        }
        // Get the signed transaction.
        let commit_tx = commit_tx_sighasher.into_transaction();
        let commit_raw_tx = commit_tx.raw_hex().to_string();
        let commit_txid = commit_tx.compute_txid();

        // *********** START CREATING REVEAL TRANSACTION ********************

        let fee_payer_utxo_input = self.reveal_transaction_normal_input(
            &self.user_key.as_ref().context("User key is not set")?.wpkh,
            commit_txid,
            commit_change_value,
        )?;

        let reveal_p2tr_input = self.reveal_transaction_p2tr_input(
            &self
                .inscription_data
                .as_ref()
                .context("Inscription data is not set")?
                .inscription_script,
            commit_txid,
            self.inscription_data
                .as_ref()
                .context("Inscription data is not set")?
                .taproot_spend_info
                .clone(),
        )?;

        let normal_input = TxIn {
            previous_output: fee_payer_utxo_input.0, // The dummy output we are spending.
            script_sig: ScriptBuf::default(),        // For a p2wpkh script_sig is empty.
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::default(), // Filled in after signing.
        };

        let reveal_input = TxIn {
            previous_output: reveal_p2tr_input.0, // The dummy output we are spending.
            script_sig: ScriptBuf::default(),     // For a p2tr script_sig is empty.
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::default(), // Filled in after signing.
        };

        let reveal_tx_estimate_size = self.estimate_transaction_size(
            1,
            1,
            1,
            0,
            vec![
                self.inscription_data
                    .as_ref()
                    .context("Inscription data is not set")?
                    .script_size,
            ],
        );

        let reveal_fee = fee_rate * reveal_tx_estimate_size as u64;
        let reveal_fee = Amount::from_sat(reveal_fee);

        let reveal_change_value = fee_payer_utxo_input.1.value - reveal_fee;

        let reveal_change_output = TxOut {
            value: reveal_change_value,
            script_pubkey: ScriptBuf::new_p2wpkh(
                &self.user_key.as_ref().context("User key is not set")?.wpkh,
            ),
        };

        // The transaction we want to sign and broadcast.
        let mut unsigned_reveal_tx = Transaction {
            version: transaction::Version::TWO,      // Post BIP-68.
            lock_time: absolute::LockTime::ZERO,     // Ignore the locktime.
            input: vec![normal_input, reveal_input], // Input goes into index 0.
            output: vec![reveal_change_output],      // Outputs, order does not matter.
        };

        let fee_input_index = 0;
        let reveal_input_index = 1;

        let mut sighasher = SighashCache::new(&mut unsigned_reveal_tx);

        let sighash_type = EcdsaSighashType::All;

        let fee_input_sighash = sighasher
            .p2wpkh_signature_hash(
                fee_input_index,
                &fee_payer_utxo_input.1.script_pubkey,
                fee_payer_utxo_input.1.value,
                sighash_type,
            )
            .context("Failed to create sighash")?;

        // Sign the sighash using the secp256k1 library (exported by rust-bitcoin).
        let msg = Message::from(fee_input_sighash);
        let fee_input_signature = self.secp.sign_ecdsa(
            &msg,
            &self.user_key.as_ref().context("User key is not set")?.sk,
        );

        // Update the witness stack.
        let fee_input_signature = bitcoin::ecdsa::Signature {
            signature: fee_input_signature,
            sighash_type,
        };

        let pk = self
            .user_key
            .as_ref()
            .context("User key is not set")?
            .sk
            .public_key(&self.secp);

        *sighasher
            .witness_mut(fee_input_index)
            .ok_or_else(|| anyhow::anyhow!("Failed to get witness"))? =
            Witness::p2wpkh(&fee_input_signature, &pk);

        // **Sign the reveal input**

        let sighash_type = TapSighashType::All;
        let prevouts = [fee_payer_utxo_input.1, reveal_p2tr_input.1];
        let prevouts = Prevouts::All(&prevouts);

        let reveal_input_sighash = sighasher
            .taproot_script_spend_signature_hash(
                reveal_input_index,
                &prevouts,
                TapLeafHash::from_script(
                    &self
                        .inscription_data
                        .as_ref()
                        .context("Inscription data is not set")?
                        .inscription_script,
                    LeafVersion::TapScript,
                ),
                sighash_type,
            )
            .context("Failed to create sighash")?;

        // Sign the sighash using the secp256k1 library (exported by rust-bitcoin).
        let msg = Message::from_digest(reveal_input_sighash.to_byte_array());
        let reveal_input_signature = self.secp.sign_schnorr_no_aux_rand(
            &msg,
            &self
                .user_key
                .as_ref()
                .context("User key is not set")?
                .keypair,
        );

        // verify
        self.secp
            .verify_schnorr(
                &reveal_input_signature,
                &msg,
                &self
                    .user_key
                    .as_ref()
                    .context("User key is not set")?
                    .internal_key,
            )
            .context("Failed to verify the signature")?;

        // Update the witness stack.
        let reveal_input_signature = bitcoin::taproot::Signature {
            signature: reveal_input_signature,
            sighash_type,
        };

        let mut witness_data: Witness = Witness::new();

        witness_data.push(&reveal_input_signature.to_vec());
        witness_data.push(
            &self
                .inscription_data
                .as_ref()
                .context("Inscription data is not set")?
                .inscription_script
                .to_bytes(),
        );

        // add control block
        witness_data.push(&reveal_p2tr_input.2.serialize());

        *sighasher
            .witness_mut(reveal_input_index)
            .ok_or_else(|| anyhow::anyhow!("Failed to get witness"))? = witness_data;

        // Get the signed transaction.
        let reveal_tx = sighasher.into_transaction();
        let reveal_raw_tx = reveal_tx.raw_hex().to_string();
        let reveal_txid = reveal_tx.compute_txid();

        Ok(InscriptionManagerResponse {
            commit_raw_tx,
            commit_txid: commit_txid.to_string(),
            commit_fee: commit_estimated_fee,
            commit_tx_size: estimated_commitment_tx_size,
            commit_change_value,
            commit_input_count: commit_tx_input_info.inputs_count,
            commit_output_count: 2,
            reveal_raw_tx,
            reveal_txid: reveal_txid.to_string(),
            reveal_fee,
            reveal_tx_size: reveal_tx_estimate_size,
            reveal_change_value,
            reveal_input_count: 2,
            reveal_output_count: 1,
        })
    }
}

struct CliManager<'a> {
    render_config: RenderConfig<'a>,
}
impl<'a> CliManager<'a> {
    pub fn new() -> Self {
        let render_config = RenderConfig {
            prompt: StyleSheet::new().with_fg(Color::Grey),
            ..RenderConfig::default()
        };

        CliManager { render_config }
    }

    pub fn get_user_input(&self) -> Result<(String, String)> {
        let greeting_content = r#"
        Welcome to the Via Inscription CLI

        This CLI will help you to create a commitment and reveal transaction for the inscription data on the Bitcoin Testnet.

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
        
        4- Get the private key (WIF format) of the address you want to use to create the commitment and reveal transactions.

        5- Prepare the data you want to inscribe.

        **Please make sure you have done the above steps before continuing.**

        ************************************************
        when you are ready, press enter to continue...
        "#;

        let greeting = Text::new(greeting_content)
            .with_render_config(self.render_config)
            .prompt();

        if greeting.is_err() {
            return Err(anyhow::anyhow!("Greeting failed"));
        }

        let wif_string = Text::new("Enter your private key (WIF format):")
            .with_render_config(self.render_config)
            .prompt()
            .context("Failed to get the private key")?;

        let inscription_data = Text::new("Enter the data to inscribe:")
            .with_render_config(self.render_config)
            .prompt()
            .context("Failed to get the inscription data")?;

        Ok((
            wif_string.trim().to_string(),
            inscription_data.trim().to_string(),
        ))
    }

    pub fn print_result(&self, result: InscriptionManagerResponse) {
        println!("****************** Commit Transaction ******************");
        println!("Transaction ID: {}", result.commit_txid);
        println!("Fee: {}", result.commit_fee);
        println!("Transaction Size: {}", result.commit_tx_size);
        println!("Change Value: {}", result.commit_change_value);
        println!(
            "Input Count: {} Output Count: {}",
            result.commit_input_count, result.commit_output_count
        );
        println!("Raw Transaction: {}", result.commit_raw_tx);

        println!("****************** Reveal Transaction ******************");
        println!("Transaction ID: {}", result.reveal_txid);
        println!("Fee: {}", result.reveal_fee);
        println!("Transaction Size: {}", result.reveal_tx_size);
        println!("Change Value: {}", result.reveal_change_value);
        println!(
            "Input Count: {} Output Count: {}",
            result.reveal_input_count, result.reveal_output_count
        );
        println!("Raw Transaction: {}", result.reveal_raw_tx);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut inscription_manager = InscriptionManager::new(
        Network::Testnet,
        "https://api.blockcypher.com/v1/btc/test3/addrs".to_string(),
        "https://mempool.space/testnet/api/v1/fees/recommended".to_string(),
    );

    let cli_manager = CliManager::new();
    let (wif_string, inscription_data) = cli_manager.get_user_input()?;

    inscription_manager.proccess_user_input(&wif_string, &inscription_data)?;

    let result = inscription_manager.start().await?;

    cli_manager.print_result(result);

    Ok(())
}
