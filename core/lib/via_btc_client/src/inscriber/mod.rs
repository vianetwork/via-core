#![allow(dead_code)]

use std::collections::HashMap;

use anyhow::{Context, Result};
use bitcoin::{
    absolute,
    hashes::Hash,
    sighash::{Prevouts, SighashCache},
    taproot::{ControlBlock, LeafVersion},
    transaction, Address, Amount, EcdsaSighashType, OutPoint, ScriptBuf, Sequence, TapLeafHash,
    TapSighashType, Transaction, TxIn, TxOut, Witness,
};
use bitcoincore_rpc::{Auth, RawTx};
use secp256k1::Message;
use tracing::{debug, info, instrument, warn};

use crate::{
    client::BitcoinClient,
    inscriber::{
        fee::InscriberFeeCalculator,
        internal_type::{
            CommitTxInputRes, CommitTxOutputRes, FinalTx, RevealTxInputRes, RevealTxOutputRes,
        },
        script_builder::InscriptionData,
    },
    signer::KeyManager,
    traits::{BitcoinOps, BitcoinSigner},
    types::{InscriberContext, InscriptionMessage, Network},
};

mod fee;
mod internal_type;
mod script_builder;

const CTX_REQUIRED_CONFIRMATIONS: u32 = 1;
const FEE_RATE_CONF_TARGET: u16 = 1;

const COMMIT_TX_CHANGE_OUTPUT_INDEX: u32 = 0;
const COMMIT_TX_TAPSCRIPT_OUTPUT_INDEX: u32 = 1;
const REVEAL_TX_CHANGE_OUTPUT_INDEX: u32 = 0;
const REVEAL_TX_FEE_INPUT_INDEX: u32 = 0;
const REVEAL_TX_TAPSCRIPT_REVEAL_INDEX: u32 = 1;

const FEE_RATE_INCREASE_PER_PENDING_TX: u64 = 5; // percentage

const COMMIT_TX_P2TR_OUTPUT_COUNT: u32 = 1;
const COMMIT_TX_P2WPKH_OUTPUT_COUNT: u32 = 1;
const COMMIT_TX_P2TR_INPUT_COUNT: u32 = 0;
const REVEAL_TX_P2TR_OUTPUT_COUNT: u32 = 0;
const REVEAL_TX_P2WPKH_OUTPUT_COUNT: u32 = 1;
const REVEAL_TX_P2TR_INPUT_COUNT: u32 = 1;
const REVEAL_TX_P2WPKH_INPUT_COUNT: u32 = 1;

const BROADCAST_RETRY_COUNT: u32 = 3;

struct Inscriber {
    client: Box<dyn BitcoinOps>,
    signer: Box<dyn BitcoinSigner>,
    context: InscriberContext,
}

impl Inscriber {
    #[instrument(
        skip(rpc_url, auth, signer_private_key, persisted_ctx),
        target = "bitcoin_inscriber"
    )]
    pub async fn new(
        rpc_url: &str,
        network: Network,
        auth: Auth,
        signer_private_key: &str,
        persisted_ctx: Option<InscriberContext>,
    ) -> Result<Self> {
        info!("Creating new Inscriber");
        let client = Box::new(BitcoinClient::new(rpc_url, network, auth)?);
        let signer = Box::new(KeyManager::new(signer_private_key, network)?);
        let context = persisted_ctx.unwrap_or_default();

        Ok(Self {
            client,
            signer,
            context,
        })
    }

    // the inscribe should provide report for upper layer to give them information for updates on the transactions
    // {
    //    "consumed_utxos": [],
    //    "commit_tx": {},
    //    "reveal_tx": {},
    //    "tx_incldued_in_block": []
    // }
    #[instrument(skip(self, input), target = "bitcoin_inscriber")]
    pub async fn inscribe(&mut self, input: InscriptionMessage) -> Result<()> {
        info!("Starting inscription process");
        self.sync_context_with_blockchain().await?;

        let secp_ref = &self.signer.get_secp_ref();
        let internal_key = self.signer.get_internal_key()?;
        let network = self.client.get_network();

        let inscription_data = InscriptionData::new(&input, secp_ref, internal_key, network)?;

        let commit_tx_input_info = self.prepare_commit_tx_input().await?;

        let commit_tx_output_info = self
            .prepare_commit_tx_output(
                &commit_tx_input_info,
                inscription_data.script_pubkey.clone(),
            )
            .await?;

        let final_commit_tx = self.sign_commit_tx(&commit_tx_input_info, &commit_tx_output_info)?;

        let reveal_tx_input_info = self.prepare_reveal_tx_input(
            &commit_tx_output_info,
            &final_commit_tx,
            &inscription_data,
        )?;

        let reveal_tx_output_info = self
            .prepare_reveal_tx_output(&reveal_tx_input_info, &inscription_data)
            .await?;

        let final_reveal_tx = self.sign_reveal_tx(
            &reveal_tx_input_info,
            &reveal_tx_output_info,
            &inscription_data,
        )?;

        self.broadcast_inscription(&final_commit_tx, &final_reveal_tx)
            .await?;

        self.insert_inscription_to_context(
            input,
            final_commit_tx,
            final_reveal_tx,
            commit_tx_output_info,
            reveal_tx_output_info,
            commit_tx_input_info,
        )?;

        info!("Inscription process completed successfully");
        Ok(())
    }

    #[instrument(skip(self), target = "bitcoin_inscriber")]
    async fn sync_context_with_blockchain(&mut self) -> Result<()> {
        debug!("Syncing context with blockchain");
        if self.context.fifo_queue.is_empty() {
            debug!("Context queue is empty, no sync needed");
            return Ok(());
        }

        while let Some(inscription) = self.context.fifo_queue.pop_front() {
            let txid_ref = &inscription.fee_payer_ctx.fee_payer_utxo_txid;
            let res = self
                .client
                .check_tx_confirmation(txid_ref, CTX_REQUIRED_CONFIRMATIONS)
                .await?;

            if !res {
                debug!("Transaction not confirmed, adding back to queue");
                self.context.fifo_queue.push_front(inscription);
                break;
            }
        }

        debug!("Context sync completed");
        Ok(())
    }

    #[instrument(skip(self), target = "bitcoin_inscriber")]
    async fn prepare_commit_tx_input(&self) -> Result<CommitTxInputRes> {
        debug!("Preparing commit transaction input");
        let mut commit_tx_inputs: Vec<TxIn> = Vec::new();
        let mut unlocked_value: Amount = Amount::ZERO;
        let mut inputs_count: u32 = 0;
        let mut utxo_amounts: Vec<Amount> = Vec::new();

        let address_ref = &self.signer.get_p2wpkh_address()?;
        let mut utxos = self.client.fetch_utxos(address_ref).await?;

        /*
            adjust utxos list based on unconfirmed utxos in context

            !!! Only Service should send transaction with this address otherwise it will cause a problem in this code !!!
        */

        let context_queue_len = self.context.fifo_queue.len();

        let mut spent_utxos: HashMap<OutPoint, bool> = HashMap::new();

        for i in 0..context_queue_len {
            let inscription_req =
                self.context.fifo_queue.get(i).ok_or_else(|| {
                    anyhow::anyhow!("Failed to get inscription request from context")
                })?;

            let commit_input = inscription_req.commit_tx_input.clone();
            let commit_input_count = commit_input.spent_utxo.len();

            for j in 0..commit_input_count {
                let tx_in = commit_input.spent_utxo.get(j);

                if let Some(tx_in) = tx_in {
                    let outpoint = tx_in.previous_output;

                    spent_utxos.insert(outpoint, true);
                }
            }

            let reveal_fee_payer_input = inscription_req.inscriber_output.commit_txid;

            let reveal_fee_payer_input = OutPoint {
                txid: reveal_fee_payer_input,
                vout: REVEAL_TX_FEE_INPUT_INDEX,
            };

            spent_utxos.insert(reveal_fee_payer_input, true);

            if i != context_queue_len - 1 {
                let reveal_tx_change_output = inscription_req.inscriber_output.reveal_txid;

                let reveal_tx_change_output = OutPoint {
                    txid: reveal_tx_change_output,
                    vout: REVEAL_TX_CHANGE_OUTPUT_INDEX,
                };

                spent_utxos.insert(reveal_tx_change_output, true);
            }
        }

        // iterate over utxos and filter out spent utxos and non p2wpkh utxos

        utxos.retain(|utxo| {
            let is_spent = spent_utxos.contains_key(&utxo.0);
            let is_p2wpkh = self.is_p2wpkh(&utxo.1.script_pubkey);

            !is_spent && is_p2wpkh
        });

        // add context available utxo (head utxo) to spendable utxos list
        if context_queue_len > 0 {
            if let Some(head_inscription) = self.context.fifo_queue.front() {
                let reveal_change_output = head_inscription.inscriber_output.reveal_txid;

                let reveal_change_output = OutPoint {
                    txid: reveal_change_output,
                    vout: head_inscription.fee_payer_ctx.fee_payer_utxo_vout,
                };

                let reveal_txout = TxOut {
                    value: head_inscription.fee_payer_ctx.fee_payer_utxo_value,
                    script_pubkey: self.signer.get_p2wpkh_script_pubkey().clone(),
                };

                utxos.push((reveal_change_output, reveal_txout));
            }
        }

        for (outpoint, txout) in utxos {
            let txin = TxIn {
                previous_output: outpoint,
                script_sig: ScriptBuf::default(), // For a p2wpkh script_sig is empty.
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(), // Get filled in after signing.
            };

            commit_tx_inputs.push(txin);
            unlocked_value += txout.value;
            inputs_count += 1;
            utxo_amounts.push(txout.value);
        }

        debug!("Commit transaction input prepared");

        let res = CommitTxInputRes {
            commit_tx_inputs,
            unlocked_value,
            inputs_count,
            utxo_amounts,
        };

        Ok(res)
    }

    #[instrument(skip(self, script_pubkey), target = "bitcoin_inscriber")]
    fn is_p2wpkh(&self, script_pubkey: &ScriptBuf) -> bool {
        let p2wpkh_script = self.signer.get_p2wpkh_script_pubkey();
        script_pubkey == p2wpkh_script
    }

    #[instrument(
        skip(self, tx_input_data, inscription_pubkey),
        target = "bitcoin_inscriber"
    )]
    async fn prepare_commit_tx_output(
        &self,
        tx_input_data: &CommitTxInputRes,
        inscription_pubkey: ScriptBuf,
    ) -> Result<CommitTxOutputRes> {
        debug!("Preparing commit transaction output");
        let inscription_commitment_output = TxOut {
            value: Amount::ZERO,
            script_pubkey: inscription_pubkey,
        };

        let mut fee_rate = self.get_fee_rate().await?;
        let pending_tx_in_context = self.context.fifo_queue.len();

        // increase fee rate based on pending transactions in context

        let increase_factor = FEE_RATE_INCREASE_PER_PENDING_TX * pending_tx_in_context as u64;
        fee_rate += fee_rate * increase_factor / 100;

        let fee_amount = InscriberFeeCalculator::estimate_fee(
            tx_input_data.inputs_count,
            COMMIT_TX_P2TR_INPUT_COUNT,
            COMMIT_TX_P2WPKH_OUTPUT_COUNT,
            COMMIT_TX_P2TR_OUTPUT_COUNT,
            vec![],
            fee_rate,
        )?;

        let commit_tx_change_output = TxOut {
            value: tx_input_data.unlocked_value - fee_amount,
            script_pubkey: self.signer.get_p2wpkh_script_pubkey().clone(),
        };

        debug!("Commit transaction output prepared");

        let res = CommitTxOutputRes {
            commit_tx_change_output,
            commit_tx_tapscript_output: inscription_commitment_output,
            commit_tx_fee_rate: fee_rate,
            _commit_tx_fee: fee_amount,
        };

        Ok(res)
    }

    #[instrument(skip(self), target = "bitcoin_inscriber")]
    async fn get_fee_rate(&self) -> Result<u64> {
        debug!("Getting fee rate");
        let res = self.client.get_fee_rate(FEE_RATE_CONF_TARGET).await?;
        debug!("Fee rate obtained: {}", res);
        Ok(res)
    }

    #[instrument(skip(self, input, output), target = "bitcoin_inscriber")]
    fn sign_commit_tx(
        &self,
        input: &CommitTxInputRes,
        output: &CommitTxOutputRes,
    ) -> Result<FinalTx> {
        debug!("Signing commit transaction");
        let mut commit_outputs: [TxOut; 2] = [TxOut::NULL, TxOut::NULL];

        commit_outputs[COMMIT_TX_CHANGE_OUTPUT_INDEX as usize] =
            output.commit_tx_change_output.clone();
        commit_outputs[COMMIT_TX_TAPSCRIPT_OUTPUT_INDEX as usize] =
            output.commit_tx_tapscript_output.clone();

        let mut unsigned_commit_tx = Transaction {
            version: transaction::Version::TWO,  // Post BIP-68.
            lock_time: absolute::LockTime::ZERO, // Ignore the locktime.
            input: input.commit_tx_inputs.clone(),
            output: commit_outputs.to_vec(), // Outputs, order does not matter.
        };

        let sighash_type = EcdsaSighashType::All;
        let mut commit_tx_sighasher = SighashCache::new(&mut unsigned_commit_tx);

        let script_pubkey = self.signer.get_p2wpkh_script_pubkey();

        let commit_tx_input_len = input.commit_tx_inputs.len();
        for index in 0..commit_tx_input_len {
            let sighash = commit_tx_sighasher
                .p2wpkh_signature_hash(
                    index,
                    script_pubkey,
                    input.utxo_amounts[index],
                    sighash_type,
                )
                .context("Failed to create sighash")?;

            // Sign the sighash using the signer
            let msg = Message::from(sighash);
            let signature = self.signer.sign_ecdsa(msg)?;

            // Update the witness stack.
            let signature = bitcoin::ecdsa::Signature {
                signature,
                sighash_type,
            };
            let pk = self.signer.get_public_key();

            *commit_tx_sighasher
                .witness_mut(index)
                .ok_or_else(|| anyhow::anyhow!("Failed to get witness"))? =
                Witness::p2wpkh(&signature, &pk);
        }

        let commit_tx = commit_tx_sighasher.into_transaction();
        let txid = commit_tx.compute_txid();

        debug!("Commit transaction signed");

        let res = FinalTx {
            tx: commit_tx.clone(),
            txid,
        };

        Ok(res)
    }

    #[instrument(skip(self, commit_tx, inscription_data), target = "bitcoin_inscriber")]
    fn prepare_reveal_tx_input(
        &self,
        commit_output: &CommitTxOutputRes,
        commit_tx: &FinalTx,
        inscription_data: &InscriptionData,
    ) -> Result<RevealTxInputRes> {
        debug!("Preparing reveal transaction input");
        let p2wpkh_script_pubkey = self.signer.get_p2wpkh_script_pubkey();

        let fee_payer_utxo_input: (OutPoint, TxOut) = (
            OutPoint {
                txid: commit_tx.txid,
                vout: COMMIT_TX_CHANGE_OUTPUT_INDEX,
            },
            TxOut {
                value: commit_output.commit_tx_change_output.value,
                script_pubkey: p2wpkh_script_pubkey.clone(),
            },
        );

        let control_block = inscription_data
            .taproot_spend_info
            .control_block(&(
                inscription_data.script_pubkey.clone(),
                LeafVersion::TapScript,
            ))
            .ok_or_else(|| anyhow::anyhow!("Failed to get control block"))?;

        let network = self.client.get_network();

        let tapproot_address =
            Address::p2tr_tweaked(inscription_data.taproot_spend_info.output_key(), network);

        let reveal_p2tr_utxo_input: (OutPoint, TxOut, ControlBlock) = (
            OutPoint {
                txid: commit_tx.txid,
                vout: COMMIT_TX_TAPSCRIPT_OUTPUT_INDEX,
            },
            TxOut {
                value: commit_output.commit_tx_tapscript_output.value,
                script_pubkey: tapproot_address.script_pubkey(),
            },
            control_block,
        );

        let fee_payer_input = TxIn {
            previous_output: fee_payer_utxo_input.0,
            script_sig: ScriptBuf::default(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::default(),
        };

        let reveal_p2tr_input = TxIn {
            previous_output: reveal_p2tr_utxo_input.0,
            script_sig: ScriptBuf::default(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::default(),
        };

        let unlock_value = fee_payer_utxo_input.1.value + reveal_p2tr_utxo_input.1.value;

        let mut reveal_tx_inputs: [TxIn; 2] = [TxIn::default(), TxIn::default()];

        reveal_tx_inputs[REVEAL_TX_FEE_INPUT_INDEX as usize] = fee_payer_input;
        reveal_tx_inputs[REVEAL_TX_TAPSCRIPT_REVEAL_INDEX as usize] = reveal_p2tr_input;

        let mut prev_outs: [TxOut; 2] = [TxOut::NULL, TxOut::NULL];

        prev_outs[REVEAL_TX_FEE_INPUT_INDEX as usize] = fee_payer_utxo_input.1;
        prev_outs[REVEAL_TX_TAPSCRIPT_REVEAL_INDEX as usize] = reveal_p2tr_utxo_input.1;

        debug!("Reveal transaction input prepared");

        let res = RevealTxInputRes {
            reveal_tx_input: reveal_tx_inputs.to_vec(),
            prev_outs: prev_outs.to_vec(),
            unlock_value,
            control_block: reveal_p2tr_utxo_input.2,
        };

        Ok(res)
    }

    #[instrument(
        skip(self, tx_input_data, inscription_data),
        target = "bitcoin_inscriber"
    )]
    async fn prepare_reveal_tx_output(
        &self,
        tx_input_data: &RevealTxInputRes,
        inscription_data: &InscriptionData,
    ) -> Result<RevealTxOutputRes> {
        debug!("Preparing reveal transaction output");
        let fee_rate = self.get_fee_rate().await?;

        let fee_amount = InscriberFeeCalculator::estimate_fee(
            REVEAL_TX_P2WPKH_INPUT_COUNT,
            REVEAL_TX_P2TR_INPUT_COUNT,
            REVEAL_TX_P2WPKH_OUTPUT_COUNT,
            REVEAL_TX_P2TR_OUTPUT_COUNT,
            vec![inscription_data.script_size],
            fee_rate,
        )?;

        let reveal_change_amount = tx_input_data.unlock_value - fee_amount;

        let reveal_tx_change_output = TxOut {
            value: reveal_change_amount,
            script_pubkey: self.signer.get_p2wpkh_script_pubkey().clone(),
        };

        debug!("Reveal transaction output prepared");

        let res = RevealTxOutputRes {
            reveal_tx_change_output,
            reveal_fee_rate: fee_rate,
            _reveal_fee: fee_amount,
        };

        Ok(res)
    }

    #[instrument(
        skip(self, input, output, inscription_data),
        target = "bitcoin_inscriber"
    )]
    fn sign_reveal_tx(
        &self,
        input: &RevealTxInputRes,
        output: &RevealTxOutputRes,
        inscription_data: &InscriptionData,
    ) -> Result<FinalTx> {
        debug!("Signing reveal transaction");
        let mut unsigned_reveal_tx = Transaction {
            version: transaction::Version::TWO,  // Post BIP-68.
            lock_time: absolute::LockTime::ZERO, // Ignore the locktime.
            input: input.reveal_tx_input.clone(),
            output: vec![output.reveal_tx_change_output.clone()],
        };

        let mut sighasher = SighashCache::new(&mut unsigned_reveal_tx);
        let sighash_type = EcdsaSighashType::All;

        let script_pubkey = self.signer.get_p2wpkh_script_pubkey();

        let fee_payer_input_sighash = sighasher
            .p2wpkh_signature_hash(
                REVEAL_TX_FEE_INPUT_INDEX as usize,
                script_pubkey,
                input.unlock_value,
                sighash_type,
            )
            .context("Failed to create sighash")?;

        // Sign the fee payer sighash using the signer
        let fee_payer_msg = Message::from(fee_payer_input_sighash);
        let fee_payer_signature = self.signer.sign_ecdsa(fee_payer_msg)?;

        // Update the witness stack.

        let fee_payer_signature = bitcoin::ecdsa::Signature {
            signature: fee_payer_signature,
            sighash_type,
        };

        let fee_payer_pk = self.signer.get_public_key();

        *sighasher
            .witness_mut(REVEAL_TX_FEE_INPUT_INDEX as usize)
            .ok_or_else(|| anyhow::anyhow!("Failed to get witness"))? =
            Witness::p2wpkh(&fee_payer_signature, &fee_payer_pk);

        // sign tapscript reveal input

        let sighash_type = TapSighashType::All;
        let prevouts = Prevouts::All(&input.prev_outs);

        let reveal_input_sighash = sighasher
            .taproot_script_spend_signature_hash(
                REVEAL_TX_TAPSCRIPT_REVEAL_INDEX as usize,
                &prevouts,
                TapLeafHash::from_script(
                    &inscription_data.inscription_script,
                    LeafVersion::TapScript,
                ),
                sighash_type,
            )
            .context("Failed to create sighash")?;

        // Sign the tapscript reveal sighash using the signer
        let msg = Message::from_digest(reveal_input_sighash.to_byte_array());
        let reveal_input_signature = self.signer.sign_schnorr(msg)?;

        // verify signature
        let internal_key = self.signer.get_internal_key()?;
        let secp_ref = self.signer.get_secp_ref();

        secp_ref
            .verify_schnorr(&reveal_input_signature, &msg, &internal_key)
            .context("Failed to verify signature")?;

        // Update the witness stack.

        let reveal_input_signature = bitcoin::taproot::Signature {
            signature: reveal_input_signature,
            sighash_type,
        };

        let mut witness_data: Witness = Witness::new();

        witness_data.push(&reveal_input_signature.to_vec());
        witness_data.push(&inscription_data.inscription_script.to_bytes());

        // add control block to witness
        let control_block = input.control_block.clone();
        witness_data.push(&control_block.serialize());

        *sighasher
            .witness_mut(REVEAL_TX_TAPSCRIPT_REVEAL_INDEX as usize)
            .ok_or_else(|| anyhow::anyhow!("Failed to get witness"))? = witness_data;

        let reveal_tx = sighasher.into_transaction();

        debug!("Reveal transaction signed");

        let res = FinalTx {
            tx: reveal_tx.clone(),
            txid: reveal_tx.compute_txid(),
        };

        Ok(res)
    }

    #[instrument(skip(self, commit, reveal), target = "bitcoin_inscriber")]
    async fn broadcast_inscription(&self, commit: &FinalTx, reveal: &FinalTx) -> Result<()> {
        info!("Broadcasting inscription transactions");
        let commit_tx_hex = commit.tx.raw_hex().to_string();
        let reveal_tx_hex = reveal.tx.raw_hex().to_string();

        let commit_tx_id = self
            .client
            .broadcast_signed_transaction(&commit_tx_hex)
            .await?;
        let reveal_tx_id = self
            .client
            .broadcast_signed_transaction(&reveal_tx_hex)
            .await?;

        info!("Both transactions broadcasted successfully with ids: commit: {commit_tx_id}, reveal: {reveal_tx_id}");

        Ok(())
    }

    #[instrument(
        skip(
            self,
            req,
            commit,
            reveal,
            commit_output_info,
            reveal_output_info,
            commit_input_info
        ),
        target = "bitcoin_inscriber"
    )]
    fn insert_inscription_to_context(
        &mut self,
        req: InscriptionMessage,
        commit: FinalTx,
        reveal: FinalTx,
        commit_output_info: CommitTxOutputRes,
        reveal_output_info: RevealTxOutputRes,
        commit_input_info: CommitTxInputRes,
    ) -> Result<()> {
        debug!("Inserting inscription to context");
        let inscription_request = crate::types::InscriptionRequest {
            message: req,
            inscriber_output: crate::types::InscriberOutput {
                commit_txid: commit.txid,
                commit_raw_tx: commit.tx.raw_hex().to_string(),
                commit_tx_fee_rate: commit_output_info.commit_tx_fee_rate,
                reveal_txid: reveal.txid,
                reveal_raw_tx: reveal.tx.raw_hex().to_string(),
                reveal_tx_fee_rate: reveal_output_info.reveal_fee_rate,
                is_broadcasted: true,
            },
            fee_payer_ctx: crate::types::FeePayerCtx {
                fee_payer_utxo_txid: reveal.txid,
                fee_payer_utxo_vout: REVEAL_TX_CHANGE_OUTPUT_INDEX,
                fee_payer_utxo_value: reveal_output_info.reveal_tx_change_output.value,
            },
            commit_tx_input: crate::types::CommitTxInput {
                spent_utxo: commit_input_info.commit_tx_inputs.clone(),
            },
        };

        self.context.fifo_queue.push_back(inscription_request);
        debug!("Inscription inserted to context");

        Ok(())
    }

    #[instrument(skip(self), target = "bitcoin_inscriber")]
    pub fn get_context_snapshot(&self) -> Result<InscriberContext> {
        debug!("Getting context snapshot");
        Ok(self.context.clone())
    }

    #[instrument(skip(self, snapshot), target = "bitcoin_inscriber")]
    pub fn recreate_context_from_snapshot(&mut self, snapshot: InscriberContext) -> Result<()> {
        info!("Recreating context from snapshot");
        self.context = snapshot;
        debug!("Context recreated from snapshot");
        Ok(())
    }
}
