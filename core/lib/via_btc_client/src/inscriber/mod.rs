#![allow(dead_code)]

use std::{borrow::Borrow, collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use bitcoin::{
    absolute,
    consensus::encode::deserialize_hex,
    hashes::Hash,
    sighash::{Prevouts, SighashCache},
    taproot::{ControlBlock, LeafVersion},
    transaction, Address, Amount, EcdsaSighashType, OutPoint, ScriptBuf, Sequence, TapLeafHash,
    TapSighashType, Transaction, TxIn, TxOut, Txid, Witness,
};
use bitcoincore_rpc::RawTx;
use secp256k1::Message;
use tracing::{debug, info, instrument, warn};

use crate::{
    client::BitcoinClient,
    inscriber::{
        fee::InscriberFeeCalculator,
        internal_type::{
            CommitTxInputRes, CommitTxOutputRes, FinalTx, InscriberInfo, RevealTxInputRes,
            RevealTxOutputRes,
        },
        script_builder::InscriptionData,
    },
    signer::KeyManager,
    traits::{BitcoinOps, BitcoinSigner},
    types::{InscriberContext, InscriptionMessage, Recipient},
};

mod fee;
mod internal_type;
mod script_builder;
pub mod test_utils;

const CTX_REQUIRED_CONFIRMATIONS: u32 = 1;
const FEE_RATE_CONF_TARGET: u16 = 10;

const COMMIT_TX_CHANGE_OUTPUT_INDEX: u32 = 0;
const COMMIT_TX_TAPSCRIPT_OUTPUT_INDEX: u32 = 1;
const REVEAL_TX_CHANGE_OUTPUT_INDEX: u32 = 0;
const REVEAL_TX_FEE_INPUT_INDEX: u32 = 0;
const REVEAL_TX_TAPSCRIPT_REVEAL_INDEX: u32 = 1;

const FEE_RATE_INCREASE_PER_PENDING_TX: u64 = 5; // percentage
/// The fee percentage amount reduced from the commit transaction and added to the reveal transaction.
const FEE_RATE_DECREASE_COMMIT_TX: u64 = 20;
/// The additional fee percentage added to the base reveal transaction fee serves as an incentive for the minter to include commit and reveal transactions.
const FEE_RATE_INCENTIVE: u64 = 5;

const COMMIT_TX_P2TR_OUTPUT_COUNT: u32 = 1;
const COMMIT_TX_P2WPKH_OUTPUT_COUNT: u32 = 1;
const COMMIT_TX_P2TR_INPUT_COUNT: u32 = 0;
const REVEAL_TX_P2TR_OUTPUT_COUNT: u32 = 0;
const REVEAL_TX_P2WPKH_OUTPUT_COUNT: u32 = 1;
const REVEAL_TX_P2TR_INPUT_COUNT: u32 = 1;
const REVEAL_TX_P2WPKH_INPUT_COUNT: u32 = 1;

const BROADCAST_RETRY_COUNT: u32 = 3;

// https://bitcoin.stackexchange.com/questions/10986/what-is-meant-by-bitcoin-dust
// https://bitcointalk.org/index.php?topic=5453107.msg62262343#msg62262343
const P2TR_DUST_LIMIT: Amount = Amount::from_sat(330);

/// Keep inscription outputs comfortably above the dust floor so relay / mining policy
/// does not hinge on borderline values.
const MIN_INSCRIPTION_OUTPUT: Amount = Amount::from_sat(600);

/// Keep change outputs large enough to remain useful for follow-up reveal / chained spends.
const MIN_CHANGE_OUTPUT: Amount = Amount::from_sat(1_000);

/// Minimum buffer for change output to ensure Reveal TX can be funded.
/// This accounts for Reveal TX fees plus safety margin.
const MIN_CHANGE_BUFFER: Amount = Amount::from_sat(10_000);

/// Maximum number of UTXOs to consider for selection (performance reasoning)
const MAX_UTXOS_TO_CONSIDER: usize = 100;

#[derive(Debug, Clone)]
pub struct InscriberPolicy {
    pub min_inscription_output: Amount,
    pub min_change_output: Amount,
    pub allow_unconfirmed_change_reuse: bool,
    pub min_feerate_sat_vb: u64,
    pub min_feerate_chained_sat_vb: u64,
    pub max_feerate_sat_vb: u64,
    pub escalation_step_sat_vb: u64,
}

impl Default for InscriberPolicy {
    fn default() -> Self {
        Self {
            min_inscription_output: Amount::from_sat(600),
            min_change_output: Amount::from_sat(1_000),
            allow_unconfirmed_change_reuse: false,
            min_feerate_sat_vb: 8,
            min_feerate_chained_sat_vb: 20,
            max_feerate_sat_vb: 80,
            escalation_step_sat_vb: 5,
        }
    }
}

impl InscriberPolicy {
    pub fn from_sats(
        min_inscription_output_sats: u64,
        min_change_output_sats: u64,
        allow_unconfirmed_change_reuse: bool,
        min_feerate_sat_vb: u64,
        min_feerate_chained_sat_vb: u64,
        max_feerate_sat_vb: u64,
        escalation_step_sat_vb: u64,
    ) -> Result<Self> {
        if min_change_output_sats < P2TR_DUST_LIMIT.to_sat() {
            anyhow::bail!(
                "Invalid policy: min_change_output_sats ({}) must be >= {}",
                min_change_output_sats,
                P2TR_DUST_LIMIT.to_sat()
            );
        }

        if max_feerate_sat_vb < min_feerate_sat_vb || max_feerate_sat_vb < min_feerate_chained_sat_vb {
            anyhow::bail!(
                "Invalid fee policy: max_feerate_sat_vb ({}) must be >= both min_feerate_sat_vb ({}) and min_feerate_chained_sat_vb ({})",
                max_feerate_sat_vb,
                min_feerate_sat_vb,
                min_feerate_chained_sat_vb
            );
        }

        Ok(Self {
            min_inscription_output: Amount::from_sat(min_inscription_output_sats),
            min_change_output: Amount::from_sat(min_change_output_sats),
            allow_unconfirmed_change_reuse,
            min_feerate_sat_vb,
            min_feerate_chained_sat_vb,
            max_feerate_sat_vb,
            escalation_step_sat_vb,
        })
    }
}

/// Calculates the minimum target amount needed for UTXO selection.
/// This includes: Commit TX fee (estimated), a safe inscription output amount, and a
/// minimum change budget that stays reusable for follow-up transactions.
fn calculate_selection_target(
    input_count: u32,
    fee_rate: u64,
    policy: &InscriberPolicy,
) -> Result<Amount> {
    let commit_fee = InscriberFeeCalculator::estimate_fee(
        input_count,
        COMMIT_TX_P2TR_INPUT_COUNT,
        COMMIT_TX_P2WPKH_OUTPUT_COUNT,
        COMMIT_TX_P2TR_OUTPUT_COUNT,
        vec![],
        fee_rate,
    )?;

    let minimum_change_budget = std::cmp::max(MIN_CHANGE_BUFFER, policy.min_change_output);
    let target = commit_fee
        .checked_add(policy.min_inscription_output)
        .and_then(|v| v.checked_add(minimum_change_budget))
        .ok_or_else(|| anyhow::anyhow!("Target amount overflow"))?;
    Ok(target)
}

/// Runs Largest-First selection over the provided candidate list.
fn select_utxos_from_candidates(
    utxos: Vec<(OutPoint, TxOut)>,
    fee_rate: u64,
    policy: &InscriberPolicy,
) -> Result<(Vec<(OutPoint, TxOut)>, Amount)> {
    let mut selected: Vec<(OutPoint, TxOut)> = Vec::new();
    let mut total_value = Amount::ZERO;

    for (outpoint, txout) in utxos {
        let value = txout.value;
        selected.push((outpoint, txout));
        total_value = total_value
            .checked_add(value)
            .ok_or_else(|| anyhow::anyhow!("Total value overflow"))?;

        let input_count = selected.len() as u32;
        let target = calculate_selection_target(input_count, fee_rate, policy)?;

        if total_value >= target {
            debug!(
                "UTXO selection complete: {} inputs, {} sats, target {} sats",
                input_count,
                total_value.to_sat(),
                target.to_sat()
            );
            return Ok((selected, total_value));
        }
    }

    let final_target = calculate_selection_target(selected.len() as u32, fee_rate, policy)?;
    Err(anyhow::anyhow!(
        "Insufficient funds: have {} sats, need {} sats",
        total_value.to_sat(),
        final_target.to_sat()
    ))
}

/// Selects UTXOs using Largest-First: sorts by value descending, picks until target met.
/// Dynamically recalculates fees as inputs are added. Ensures change for Reveal TX.
fn select_utxos(
    mut utxos: Vec<(OutPoint, TxOut)>,
    fee_rate: u64,
    policy: &InscriberPolicy,
) -> Result<(Vec<(OutPoint, TxOut)>, Amount)> {
    if utxos.is_empty() {
        return Err(anyhow::anyhow!("No UTXOs available for selection"));
    }

    // Sort by value descending (largest first)
    utxos.sort_by(|a, b| b.1.value.cmp(&a.1.value));

    if utxos.len() <= MAX_UTXOS_TO_CONSIDER {
        return select_utxos_from_candidates(utxos, fee_rate, policy);
    }

    let full_candidates = utxos.clone();
    let truncated_candidates = utxos
        .iter()
        .take(MAX_UTXOS_TO_CONSIDER)
        .cloned()
        .collect::<Vec<_>>();

    match select_utxos_from_candidates(truncated_candidates, fee_rate, policy) {
        Ok(result) => Ok(result),
        Err(err) => {
            let truncated_total = utxos
                .iter()
                .take(MAX_UTXOS_TO_CONSIDER)
                .try_fold(Amount::ZERO, |acc, (_, txout)| {
                    acc.checked_add(txout.value)
                        .ok_or_else(|| anyhow::anyhow!("overflow while computing truncated UTXO total"))
                })?;
            let full_total = full_candidates
                .iter()
                .try_fold(Amount::ZERO, |acc, (_, txout)| {
                    acc.checked_add(txout.value)
                        .ok_or_else(|| anyhow::anyhow!("overflow while computing full UTXO total"))
                })?;

            if full_total > truncated_total {
                debug!(
                    "Retrying UTXO selection with full set after truncated candidate failure: truncated_total={} sats, full_total={} sats, err={}",
                    truncated_total.to_sat(),
                    full_total.to_sat(),
                    err
                );
                select_utxos_from_candidates(full_candidates, fee_rate, policy)
            } else {
                Err(err)
            }
        }
    }
}

#[derive(Debug)]
pub struct Inscriber {
    client: Arc<dyn BitcoinOps>,
    signer: Arc<dyn BitcoinSigner>,
    context: InscriberContext,
    policy: InscriberPolicy,
}

impl Inscriber {
    #[instrument(skip(client, signer_private_key), target = "bitcoin_inscriber")]
    pub async fn new(
        client: Arc<BitcoinClient>,
        signer_private_key: &str,
        persisted_ctx: Option<InscriberContext>,
    ) -> Result<Self> {
        Self::new_with_policy(client, signer_private_key, persisted_ctx, InscriberPolicy::default()).await
    }

    pub async fn new_with_policy(
        client: Arc<BitcoinClient>,
        signer_private_key: &str,
        persisted_ctx: Option<InscriberContext>,
        policy: InscriberPolicy,
    ) -> Result<Self> {
        info!("Creating new Inscriber");
        let signer = Arc::new(KeyManager::new(
            signer_private_key,
            client.config.network(),
        )?);
        let context = persisted_ctx.unwrap_or_default();

        Ok(Self {
            client,
            signer,
            context,
            policy,
        })
    }

    #[instrument(skip(self), target = "bitcoin_inscriber")]
    pub async fn get_balance(&self) -> Result<u128> {
        debug!("Getting balance");
        let address_ref = &self.signer.get_p2wpkh_address()?;
        let mut balance = self.client.get_balance(address_ref).await?;
        debug!("Balance obtained: {}", balance);

        // Include the transactions in mempool when calculate the balance
        for inscription in &self.context.fifo_queue {
            let tx: Transaction = deserialize_hex(&inscription.inscriber_output.reveal_raw_tx)?;

            tx.output.iter().for_each(|output| {
                if output.script_pubkey == address_ref.script_pubkey() {
                    balance += output.value.to_sat() as u128;
                }
            });
        }

        Ok(balance)
    }

    #[instrument(skip(self, input), target = "bitcoin_inscriber")]
    pub async fn prepare_inscribe(
        &mut self,
        input: &InscriptionMessage,
        recipient: Option<Recipient>,
    ) -> Result<InscriberInfo> {
        let secp_ref = &self.signer.get_secp_ref();
        let internal_key = self.signer.get_internal_key()?;
        let network = self.client.get_network();

        let inscription_data = InscriptionData::new(input, secp_ref, internal_key, network)?;

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
            .prepare_reveal_tx_output(
                &reveal_tx_input_info,
                &inscription_data,
                recipient,
                commit_tx_output_info.commit_tx_fee,
            )
            .await?;

        let final_reveal_tx = self.sign_reveal_tx(
            &reveal_tx_input_info,
            &reveal_tx_output_info,
            &inscription_data,
        )?;

        Ok(InscriberInfo {
            final_commit_tx,
            final_reveal_tx,
            commit_tx_output_info,
            reveal_tx_output_info,
            commit_tx_input_info,
        })
    }

    #[instrument(skip(self, input), target = "bitcoin_inscriber")]
    pub async fn inscribe_with_recipient(
        &mut self,
        input: InscriptionMessage,
        recipient: Option<Recipient>,
    ) -> Result<InscriberInfo> {
        info!("Starting inscription process");

        let inscriber_info = self
            .prepare_inscribe(&input, recipient)
            .await
            .with_context(|| "Error prepare inscriber infos")?;

        self.broadcast_inscription(
            &inscriber_info.final_commit_tx,
            &inscriber_info.final_reveal_tx,
        )
        .await?;

        self.insert_inscription_to_context(input, inscriber_info.borrow())?;

        info!("Inscription process completed successfully");
        Ok(inscriber_info)
    }

    #[instrument(skip(self, input), target = "bitcoin_inscriber")]
    pub async fn inscribe(&mut self, input: InscriptionMessage) -> Result<InscriberInfo> {
        self.inscribe_with_recipient(input, None).await
    }

    #[instrument(skip(self), target = "bitcoin_inscriber")]
    pub async fn sync_context_with_blockchain(&mut self) -> Result<()> {
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

        // Optionally reuse the head reveal-change output from the in-memory context.
        // This is disabled by default because chaining 0-conf outputs can starve the sender of
        // trusted spendable balance and create persistent head-of-line blocking.
        if self.policy.allow_unconfirmed_change_reuse && context_queue_len > 0 {
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
        } else if context_queue_len > 0 {
            debug!(
                "Skipping reuse of unconfirmed context change output; pending context depth: {}",
                context_queue_len
            );
        }

        // Get fee rate for selection calculation
        let fee_rate = self.get_fee_rate(self.context.fifo_queue.len()).await?;

        // Select optimal UTXOs using the Largest-First selection algorithm
        let (selected_utxos, unlocked_value) = select_utxos(utxos, fee_rate, &self.policy)?;

        // Build transaction inputs from selected UTXOs
        let mut commit_tx_inputs: Vec<TxIn> = Vec::new();
        let mut utxo_amounts: Vec<Amount> = Vec::new();

        for (outpoint, txout) in selected_utxos {
            let txin = TxIn {
                previous_output: outpoint,
                script_sig: ScriptBuf::default(), // For a p2wpkh script_sig is empty.
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(), // Get filled in after signing.
            };

            commit_tx_inputs.push(txin);
            utxo_amounts.push(txout.value);
        }

        let inputs_count = commit_tx_inputs.len() as u32;
        debug!(
            "Commit transaction input prepared: {} inputs, {} sats",
            inputs_count,
            unlocked_value.to_sat()
        );

        let res = CommitTxInputRes {
            commit_tx_inputs,
            unlocked_value,
            inputs_count,
            utxo_amounts,
            fee_rate,
        };

        Ok(res)
    }

    #[instrument(skip(self, script_pubkey), target = "bitcoin_inscriber")]
    // this method checks if the script_pubkey is p2wpkh and matches with signer's p2wpkh script_pubkey or not
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
            value: std::cmp::max(P2TR_DUST_LIMIT, self.policy.min_inscription_output),
            script_pubkey: inscription_pubkey,
        };

        let fee_rate = tx_input_data.fee_rate;

        let mut fee_amount = InscriberFeeCalculator::estimate_fee(
            tx_input_data.inputs_count,
            COMMIT_TX_P2TR_INPUT_COUNT,
            COMMIT_TX_P2WPKH_OUTPUT_COUNT,
            COMMIT_TX_P2TR_OUTPUT_COUNT,
            vec![],
            fee_rate,
        )?;
        let fee_amount_before_decrease = fee_amount;
        fee_amount -= (fee_amount * FEE_RATE_DECREASE_COMMIT_TX) / 100;

        let inscription_output_value = std::cmp::max(P2TR_DUST_LIMIT, self.policy.min_inscription_output);
        let commit_tx_change_output_value = tx_input_data
            .unlocked_value
            .checked_sub(fee_amount + inscription_output_value)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Required Amount: {:?}, Spendable Amount: {:?}",
                    fee_amount + inscription_output_value,
                    tx_input_data.unlocked_value
                )
            })?;

        if commit_tx_change_output_value < self.policy.min_change_output {
            anyhow::bail!(
                "Commit change output {:?} below minimum reusable threshold {:?}",
                commit_tx_change_output_value,
                self.policy.min_change_output
            );
        }

        let commit_tx_change_output = TxOut {
            value: commit_tx_change_output_value,
            script_pubkey: self.signer.get_p2wpkh_script_pubkey().clone(),
        };

        debug!("Commit transaction output prepared");

        let res = CommitTxOutputRes {
            commit_tx_change_output,
            commit_tx_tapscript_output: inscription_commitment_output,
            commit_tx_fee_rate: fee_rate,
            commit_tx_fee: fee_amount_before_decrease,
        };

        Ok(res)
    }

    #[instrument(skip(self), target = "bitcoin_inscriber")]
    async fn get_fee_rate(&self, pending_chain_depth: usize) -> Result<u64> {
        debug!("Getting fee rate");
        let network_rate = self.client.get_fee_rate(FEE_RATE_CONF_TARGET).await?;
        let floor = if pending_chain_depth > 0 {
            self.policy.min_feerate_chained_sat_vb
        } else {
            self.policy.min_feerate_sat_vb
        };
        let escalated = floor.saturating_add(self.policy.escalation_step_sat_vb.saturating_mul(pending_chain_depth as u64));
        if self.policy.max_feerate_sat_vb < floor {
            anyhow::bail!(
                "Invalid fee policy: max_feerate_sat_vb ({}) is lower than the required floor ({})",
                self.policy.max_feerate_sat_vb,
                floor
            );
        }
        let candidate = std::cmp::max(std::cmp::max(network_rate, floor), escalated);
        let effective = std::cmp::max(
            floor,
            std::cmp::min(self.policy.max_feerate_sat_vb, candidate),
        );
        debug!(
            "Fee rate obtained: network={}, pending_depth={}, floor={}, max_feerate={}, effective={}",
            network_rate,
            pending_chain_depth,
            floor,
            self.policy.max_feerate_sat_vb,
            effective
        );
        Ok(std::cmp::max(effective, 1))
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
                .with_context(|| "Failed to create commit sighash")?;

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
                inscription_data.inscription_script.clone(),
                LeafVersion::TapScript,
            ))
            .ok_or_else(|| anyhow::anyhow!("Failed to get control block"))?;

        let network = self.client.get_network();

        let taproot_address =
            Address::p2tr_tweaked(inscription_data.taproot_spend_info.output_key(), network);

        let reveal_p2tr_utxo_input: (OutPoint, TxOut, ControlBlock) = (
            OutPoint {
                txid: commit_tx.txid,
                vout: COMMIT_TX_TAPSCRIPT_OUTPUT_INDEX,
            },
            TxOut {
                value: commit_output.commit_tx_tapscript_output.value,
                script_pubkey: taproot_address.script_pubkey(),
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
            fee_rate: commit_output.commit_tx_fee_rate,
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
        recipient: Option<Recipient>,
        commit_tx_fee: Amount,
    ) -> Result<RevealTxOutputRes> {
        debug!("Preparing reveal transaction output");
        let fee_rate = tx_input_data.fee_rate;

        let mut reveal_tx_p2wpkh_output_count = REVEAL_TX_P2WPKH_OUTPUT_COUNT;
        let mut reveal_tx_p2tr_output_count = REVEAL_TX_P2TR_OUTPUT_COUNT;

        if let Some(r) = &recipient {
            if r.address.script_pubkey().is_p2tr() {
                reveal_tx_p2tr_output_count += 1;
            } else {
                reveal_tx_p2wpkh_output_count += 1;
            };
        }

        let mut fee_amount = InscriberFeeCalculator::estimate_fee(
            REVEAL_TX_P2WPKH_INPUT_COUNT,
            REVEAL_TX_P2TR_INPUT_COUNT,
            reveal_tx_p2wpkh_output_count,
            reveal_tx_p2tr_output_count,
            vec![inscription_data.script_size],
            fee_rate,
        )?;

        let increase_factor = FEE_RATE_INCENTIVE;
        fee_amount += (fee_amount * increase_factor) / 100;
        // Add the fee amount removed from the commit tx to reveal
        fee_amount += (commit_tx_fee * FEE_RATE_DECREASE_COMMIT_TX) / 100;

        let recipient_amount = recipient.as_ref().map_or(Amount::ZERO, |r| r.amount);

        let reveal_change_amount = tx_input_data
            .unlock_value
            .checked_sub(fee_amount + recipient_amount)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Required Amount:{:?} Spendable Amount: {:?}",
                    fee_amount + recipient_amount,
                    tx_input_data.unlock_value
                )
            })?;

        if reveal_change_amount < self.policy.min_change_output {
            anyhow::bail!(
                "Reveal change output {:?} below minimum reusable threshold {:?}",
                reveal_change_amount,
                self.policy.min_change_output
            );
        }

        // Change output goes back to the inscriber
        let reveal_tx_change_output = TxOut {
            value: reveal_change_amount,
            script_pubkey: self.signer.get_p2wpkh_script_pubkey().clone(),
        };

        debug!("Reveal transaction output prepared");

        // Create the recipient output if the recipient is provided
        let recipient_tx_output = recipient.map(|r| TxOut {
            value: r.amount,
            script_pubkey: r.address.script_pubkey(),
        });

        let res = RevealTxOutputRes {
            reveal_tx_change_output,
            recipient_tx_output,
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

        // Create the transaction outputs: change output and possibly the recipient output
        let mut outputs = vec![output.reveal_tx_change_output.clone()];

        if let Some(recipient_output) = output.recipient_tx_output.clone() {
            outputs.push(recipient_output);
        }

        let mut unsigned_reveal_tx = Transaction {
            version: transaction::Version::TWO,  // Post BIP-68.
            lock_time: absolute::LockTime::ZERO, // Ignore the locktime.
            input: input.reveal_tx_input.clone(),
            output: outputs, // Outputs now include change and optional recipient output
        };

        let mut sighasher = SighashCache::new(&mut unsigned_reveal_tx);
        let sighash_type = EcdsaSighashType::All;

        let script_pubkey = self.signer.get_p2wpkh_script_pubkey();

        let fee_payer_input_sighash = sighasher
            .p2wpkh_signature_hash(
                REVEAL_TX_FEE_INPUT_INDEX as usize,
                script_pubkey,
                input.prev_outs[REVEAL_TX_FEE_INPUT_INDEX as usize].value,
                sighash_type,
            )
            .with_context(|| "Failed to create reveal sighash payer")?;

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
            .with_context(|| "Failed to create reveal sighash")?;

        // Sign the tapscript reveal sighash using the signer
        let msg = Message::from_digest(reveal_input_sighash.to_byte_array());
        let reveal_input_signature = self.signer.sign_schnorr(msg)?;

        // Update the witness stack.

        let reveal_input_signature = bitcoin::taproot::Signature {
            signature: reveal_input_signature,
            sighash_type,
        };

        let mut witness_data: Witness = Witness::new();

        witness_data.push(reveal_input_signature.to_vec());
        witness_data.push(inscription_data.inscription_script.to_bytes());

        // add control block to witness
        let control_block = input.control_block.clone();
        witness_data.push(control_block.serialize());

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
    async fn broadcast_inscription(&self, commit: &FinalTx, reveal: &FinalTx) -> Result<Txid> {
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

        Ok(reveal_tx_id)
    }

    #[instrument(skip(self, req, inscriber_info,), target = "bitcoin_inscriber")]
    fn insert_inscription_to_context(
        &mut self,
        req: InscriptionMessage,
        inscriber_info: &InscriberInfo,
    ) -> Result<()> {
        debug!("Inserting inscription to context");
        let inscription_request = crate::types::InscriptionRequest {
            message: req,
            inscriber_output: crate::types::InscriberOutput {
                commit_txid: inscriber_info.final_commit_tx.txid,
                commit_raw_tx: inscriber_info.final_commit_tx.tx.raw_hex().to_string(),
                commit_tx_fee_rate: inscriber_info.commit_tx_output_info.commit_tx_fee_rate,
                reveal_txid: inscriber_info.final_reveal_tx.txid,
                reveal_raw_tx: inscriber_info.final_reveal_tx.tx.raw_hex().to_string(),
                reveal_tx_fee_rate: inscriber_info.reveal_tx_output_info.reveal_fee_rate,
                is_broadcasted: true,
            },
            fee_payer_ctx: crate::types::FeePayerCtx {
                fee_payer_utxo_txid: inscriber_info.final_reveal_tx.txid,
                fee_payer_utxo_vout: REVEAL_TX_CHANGE_OUTPUT_INDEX,
                fee_payer_utxo_value: inscriber_info
                    .reveal_tx_output_info
                    .reveal_tx_change_output
                    .value,
            },
            commit_tx_input: crate::types::CommitTxInput {
                spent_utxo: inscriber_info.commit_tx_input_info.commit_tx_inputs.clone(),
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

    #[instrument(skip(self), target = "bitcoin_inscriber")]
    pub async fn get_client(&self) -> &dyn BitcoinOps {
        &*self.client
    }

    #[instrument(skip(self), target = "bitcoin_inscriber")]
    pub fn inscriber_address(&self) -> anyhow::Result<Address> {
        Ok(self.signer.get_p2wpkh_address()?)
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use bitcoin::{
        key::UntweakedPublicKey,
        secp256k1::{
            ecdsa::Signature as ECDSASignature, schnorr::Signature as SchnorrSignature, All,
            Keypair, Message, PublicKey, Secp256k1,
        },
        Block, BlockHash, CompressedPublicKey, OutPoint, PrivateKey, ScriptBuf, Transaction, TxOut,
        Txid,
    };
    use bitcoincore_rpc::json::GetBlockStatsResult;
    use mockall::{mock, predicate::*};

    use super::*;
    use crate::types::{
        BitcoinClientResult, BitcoinNetwork, BitcoinSignerResult, InscriptionMessage,
        L1BatchDAReferenceInput,
    };

    #[test]
    fn test_select_utxos_largest_first() {
        let script_pubkey = ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::all_zeros());

        let utxos = vec![
            (
                OutPoint { txid: Txid::all_zeros(), vout: 0 },
                TxOut { value: Amount::from_sat(1_000), script_pubkey: script_pubkey.clone() },
            ),
            (
                OutPoint { txid: Txid::all_zeros(), vout: 1 },
                TxOut { value: Amount::from_sat(50_000), script_pubkey: script_pubkey.clone() },
            ),
            (
                OutPoint { txid: Txid::all_zeros(), vout: 2 },
                TxOut { value: Amount::from_sat(10_000), script_pubkey: script_pubkey.clone() },
            ),
            (
                OutPoint { txid: Txid::all_zeros(), vout: 3 },
                TxOut { value: Amount::from_sat(100_000), script_pubkey: script_pubkey.clone() },
            ),
        ];

        // Select with a low fee rate - should prefer largest UTXOs
        let (selected, total) = select_utxos(utxos, 1, &InscriberPolicy::default()).unwrap();

        // Verify largest first ordering and no unnecessary extra inputs for this set.
        assert_eq!(selected[0].1.value, Amount::from_sat(100_000));
        assert_eq!(selected.len(), 1);
        assert_eq!(total, Amount::from_sat(100_000));
    }

    #[test]
    fn test_select_utxos_insufficient_funds() {
        let script_pubkey = ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::all_zeros());

        let utxos = vec![
            (
                OutPoint { txid: Txid::all_zeros(), vout: 0 },
                TxOut { value: Amount::from_sat(100), script_pubkey: script_pubkey.clone() },
            ),
        ];

        let result = select_utxos(utxos, 10, &InscriberPolicy::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Insufficient funds"));
    }

    #[test]
    fn test_select_utxos_empty() {
        let utxos: Vec<(OutPoint, TxOut)> = vec![];
        let result = select_utxos(utxos, 10, &InscriberPolicy::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No UTXOs available"));
    }


    #[test]
    fn test_select_utxos_falls_back_to_full_set_when_truncated_prefix_is_insufficient() {
        let script_pubkey = ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::all_zeros());

        let mut utxos = vec![];
        // First 100 entries are too small to satisfy the target.
        for vout in 0..100u32 {
            utxos.push((
                OutPoint { txid: Txid::all_zeros(), vout },
                TxOut { value: Amount::from_sat(100), script_pubkey: script_pubkey.clone() },
            ));
        }
        // The 101st entry makes the full set sufficient.
        utxos.push((
            OutPoint { txid: Txid::all_zeros(), vout: 100 },
            TxOut { value: Amount::from_sat(20_000), script_pubkey: script_pubkey.clone() },
        ));

        let (selected, total) = select_utxos(utxos, 10, &InscriberPolicy::default()).unwrap();
        assert!(selected.iter().any(|(outpoint, _)| outpoint.vout == 100));
        assert!(total.to_sat() >= 20_000);
    }

    #[test]
    fn test_calculate_selection_target() {
        let policy = InscriberPolicy::default();
        let low_fee_target =
            calculate_selection_target(1, 1, &policy).expect("low fee target calculation failed");
        let high_fee_target =
            calculate_selection_target(1, 10, &policy).expect("high fee target calculation failed");

        assert!(high_fee_target.to_sat() > low_fee_target.to_sat());
        assert!(high_fee_target.to_sat() >= policy.min_inscription_output.to_sat() + MIN_CHANGE_BUFFER.to_sat());
    }

    mock! {
        BitcoinOps {}
        #[async_trait]
        impl BitcoinOps for BitcoinOps {
            async fn get_transaction(&self, txid: &Txid) -> BitcoinClientResult<Transaction>;
            async fn fetch_block(&self, block_height: u128) -> BitcoinClientResult<Block>;
            async fn fetch_block_by_hash(&self, block_hash: &BlockHash) -> BitcoinClientResult<Block>;
            async fn get_balance(&self, address: &Address) -> BitcoinClientResult<u128>;
            async fn broadcast_signed_transaction(&self, signed_transaction: &str) -> BitcoinClientResult<Txid>;
            async fn fetch_utxos(&self, address: &Address) -> BitcoinClientResult<Vec<(OutPoint, TxOut)>>;
            async fn check_tx_confirmation(&self, txid: &Txid, conf_num: u32) -> BitcoinClientResult<bool>;
            async fn fetch_block_height(&self) -> BitcoinClientResult<u64>;
            async fn get_fee_rate(&self, conf_target: u16) -> BitcoinClientResult<u64>;
            fn get_network(&self) -> BitcoinNetwork;
            async fn get_block_stats(&self, height: u64) -> BitcoinClientResult<GetBlockStatsResult>;
            async fn get_fee_history(
                &self,
                from_block_height: usize,
                to_block_height: usize,
            ) -> BitcoinClientResult<Vec<u64>>;
        }
    }

    mock! {
        BitcoinSigner {}
        impl BitcoinSigner for BitcoinSigner {
            fn sign_ecdsa(&self, msg: Message) -> BitcoinSignerResult<ECDSASignature>;
            fn sign_schnorr(&self, msg: Message) -> BitcoinSignerResult<SchnorrSignature>;
            fn get_p2wpkh_address(&self) -> BitcoinSignerResult<Address>;
            fn get_p2wpkh_script_pubkey(&self) -> &ScriptBuf;
            fn get_secp_ref(&self) -> &Secp256k1<All>;
            fn get_internal_key(&self) -> BitcoinSignerResult<UntweakedPublicKey>;
            fn get_public_key(&self) -> PublicKey;
        }
    }

    fn get_mock_inscriber_and_conditions() -> Inscriber {
        let mut client = MockBitcoinOps::new();
        let mut signer = MockBitcoinSigner::new();
        let context = InscriberContext::default();

        // Setup signer
        let secp = Secp256k1::new();
        let sk = PrivateKey::generate(BitcoinNetwork::Regtest);
        let keypair = Keypair::from_secret_key(&secp, &sk.inner);
        let compressed_pk = CompressedPublicKey::from_private_key(&secp, &sk)
            .expect("Failed to generate compressed public key");
        let address = Address::p2wpkh(&compressed_pk, BitcoinNetwork::Regtest);
        let internal_key = keypair.x_only_public_key().0;
        let script_pubkey = address.script_pubkey();

        // Setup mock for get_secp_ref
        signer.expect_get_secp_ref().return_const(secp.clone()); // Returning a reference to a Secp256k1 instance

        // Setup mock for get_internal_key
        signer
            .expect_get_internal_key()
            .returning(move || Ok(internal_key));

        // Setup mock for get_p2wpkh_address
        signer
            .expect_get_p2wpkh_address()
            .returning(move || Ok(address.clone()));

        // Setup mock for get_p2wpkh_script_pubkey
        signer
            .expect_get_p2wpkh_script_pubkey()
            .return_const(script_pubkey.clone());

        // sign_ecdsa
        signer
            .expect_sign_ecdsa()
            .times(2)
            .returning(|_| Ok(ECDSASignature::from_compact(&[0; 64]).unwrap()));

        // sign_schnorr
        signer
            .expect_sign_schnorr()
            .times(1)
            .returning(|_| Ok(SchnorrSignature::from_slice(&[0; 64]).unwrap()));

        // get_public_key
        let pk = sk.public_key(&secp);
        signer.expect_get_public_key().return_const(pk.inner);

        // Setup Client
        client
            .expect_get_network()
            .times(2)
            .return_const(BitcoinNetwork::Regtest);

        client.expect_fetch_utxos().returning(move |_| {
            let fake_outpoint = OutPoint {
                txid: Txid::all_zeros(),
                vout: 0,
            };

            let fake_txout = TxOut {
                value: Amount::from_btc(2.0).unwrap(),
                script_pubkey: script_pubkey.clone(),
            };

            Ok(vec![(fake_outpoint, fake_txout)])
        });

        client.expect_get_fee_rate().returning(|_| Ok(1));

        client
            .expect_broadcast_signed_transaction()
            .returning(|_| Ok(Txid::all_zeros()));

        Inscriber {
            client: Arc::new(client),
            signer: Arc::new(signer),
            context,
            policy: InscriberPolicy::default(),
        }
    }

    #[tokio::test]
    async fn test_inscriber_inscribe() {
        let mut inscriber = get_mock_inscriber_and_conditions();

        let l1_da_batch_ref = L1BatchDAReferenceInput {
            l1_batch_hash: zksync_basic_types::H256([0; 32]),
            l1_batch_index: zksync_basic_types::L1BatchNumber(0_u32),
            da_identifier: "da_identifier_celestia".to_string(),
            blob_id: "batch_temp_blob_id".to_string(),
            prev_l1_batch_hash: zksync_basic_types::H256([0; 32]),
        };

        let inscribe_message = InscriptionMessage::L1BatchDAReference(l1_da_batch_ref);

        let res = inscriber.inscribe(inscribe_message).await.unwrap();

        assert_ne!(res.final_commit_tx.txid, Txid::all_zeros());
        assert_ne!(res.final_reveal_tx.txid, Txid::all_zeros());
    }
}
