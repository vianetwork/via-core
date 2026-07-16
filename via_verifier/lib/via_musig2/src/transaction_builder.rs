use std::{cmp::Ordering, collections::HashSet, sync::Arc};

use anyhow::{Context, Result};
use bitcoin::{
    absolute,
    hashes::Hash,
    script::PushBytesBuf,
    sighash::{Prevouts, SighashCache},
    transaction, Amount, OutPoint, ScriptBuf, Sequence, TapSighashType, Transaction, TxIn, TxOut,
    Witness,
};
use tracing::instrument;
use via_btc_client::traits::BitcoinOps;
use via_verifier_types::transaction::UnsignedBridgeTx;

use crate::{
    constants::{
        INPUT_BASE_SIZE, INPUT_WITNESS_SIZE, OP_RETURN_SIZE, OUTPUT_SIZE, TX_OVERHEAD,
        WITNESS_OVERHEAD,
    },
    fee::FeeStrategy,
    types::{TransactionBuilderConfig, TransactionOutput, TransactionWithFee},
    utxo_manager::UtxoManager,
};

#[derive(Debug, Clone)]
pub struct TransactionBuilder {
    pub utxo_manager: UtxoManager,
}

impl TransactionBuilder {
    #[instrument(skip(btc_client), target = "bitcoin_transaction_builder")]
    pub fn new(btc_client: Arc<dyn BitcoinOps>) -> Result<Self> {
        let utxo_manager = UtxoManager::new(btc_client, Amount::from_sat(1000), 128);
        Ok(Self { utxo_manager })
    }

    /// Builds transactions with OP_RETURN data from the provided outputs
    pub async fn build_transaction_with_op_return(
        &self,
        outputs: Vec<TransactionOutput>,
        config: TransactionBuilderConfig,
    ) -> Result<Vec<UnsignedBridgeTx>> {
        self.utxo_manager.sync_context_with_blockchain().await?;

        let available_utxos = self.get_available_utxos(&config).await?;
        let fee_rate = self.get_fee_rate(&config).await?;

        self.build_bridge_txs(available_utxos, outputs, config, fee_rate)
            .await
    }

    pub async fn utxo_manager_insert_transaction(&self, tx: Transaction) {
        self.utxo_manager.insert_transaction(tx).await;
    }

    /// Estimates the weight of a transaction based on input and output counts
    pub(crate) fn estimate_transaction_weight(&self, inputs: u64, outputs: u64) -> u64 {
        let base_size = TX_OVERHEAD
            + INPUT_BASE_SIZE * inputs
            + OUTPUT_SIZE * (outputs + 1) // include change output
            + OP_RETURN_SIZE;

        let witness_size = if inputs > 0 {
            WITNESS_OVERHEAD + INPUT_WITNESS_SIZE * inputs
        } else {
            0
        };

        base_size * 4 + witness_size
    }

    /// Builds multiple bridge transactions from chunked outputs
    pub async fn build_bridge_txs(
        &self,
        available_utxos: Vec<(OutPoint, TxOut)>,
        outputs: Vec<TransactionOutput>,
        config: TransactionBuilderConfig,
        fee_rate: u64,
    ) -> Result<Vec<UnsignedBridgeTx>> {
        anyhow::ensure!(
            config.max_output_per_tx > 0,
            "max_output_per_tx must be positive"
        );
        let mut utxos_pool = available_utxos;
        let mut bridge_txs = Vec::new();

        for (i, output_chunk) in outputs.chunks(config.max_output_per_tx).enumerate() {
            if let Some(bridge_tx) = self
                .build_single_bridge_tx(output_chunk, &mut utxos_pool, &config, fee_rate, i)
                .await?
            {
                bridge_txs.push(bridge_tx);
            }
        }

        Ok(bridge_txs)
    }

    /// Prepares transaction by selecting UTXOs and calculating fees
    pub fn prepare_build_transaction(
        &self,
        mut outputs: Vec<TransactionOutput>,
        available_utxos: &[(OutPoint, TxOut)],
        fee_rate: u64,
        fee_strategy: Arc<dyn FeeStrategy>,
    ) -> Result<(TransactionWithFee, Vec<(OutPoint, TxOut)>)> {
        let mut input_count = 1;

        // Fee-ineligible outputs stay removed so input-count convergence is monotonic.
        loop {
            let tx_fee = fee_strategy.apply_fee_to_outputs(&mut outputs, input_count, fee_rate)?;
            if tx_fee.fee == Amount::ZERO {
                anyhow::bail!("Error to prepare build transaction, fee=0");
            }
            if tx_fee.outputs_with_fees.is_empty() {
                return Ok((tx_fee, vec![]));
            }

            let total_needed = tx_fee
                .total_value_needed
                .checked_add(tx_fee.fee)
                .context("Total amount overflow during UTXO selection")?;

            let max_input_count = available_utxos.len().max(1) as u32;
            let Some(selected_utxos) =
                UtxoManager::select_utxo_prefix(available_utxos, total_needed)?
            else {
                if input_count == max_input_count {
                    anyhow::bail!(
                        "{} UTXOs cannot fund fee-eligible outputs needing {}",
                        available_utxos.len(),
                        total_needed
                    );
                }
                input_count = max_input_count;
                continue;
            };

            let next_input_count = selected_utxos.len() as u32;
            match next_input_count.cmp(&input_count) {
                Ordering::Equal => {
                    tracing::debug!("Selected UTXOs {:?}", &selected_utxos);
                    return Ok((tx_fee, selected_utxos));
                }
                Ordering::Less | Ordering::Greater => input_count = next_input_count,
            }
        }
    }

    /// Anchors a proposed transaction's prevouts and rebuilds it from gross requested outputs.
    pub async fn verify_bridge_tx(
        &self,
        candidate: &UnsignedBridgeTx,
        requested_outputs: Vec<TransactionOutput>,
        config: TransactionBuilderConfig,
    ) -> Result<bool> {
        if candidate.utxos.len() != candidate.tx.input.len() {
            return Ok(false);
        }
        if matches!(
            self.estimate_transaction_weight(candidate.tx.input.len() as u64, 0)
                .cmp(&config.max_tx_weight),
            Ordering::Greater
        ) {
            return Ok(false);
        }

        let mut seen = HashSet::with_capacity(candidate.utxos.len());
        let bridge_script = config.bridge_address.script_pubkey();
        for (input, (outpoint, supplied)) in candidate.tx.input.iter().zip(&candidate.utxos) {
            if input.previous_output != *outpoint {
                return Ok(false);
            }
            if !seen.insert(*outpoint) {
                return Ok(false);
            }
            let parent = self
                .utxo_manager
                .get_btc_client()
                .get_transaction(&outpoint.txid)
                .await
                .with_context(|| format!("Failed to fetch bridge prevout {outpoint}"))?;
            if parent.compute_txid() != outpoint.txid {
                return Ok(false);
            }
            if parent.output.get(outpoint.vout as usize) != Some(supplied) {
                return Ok(false);
            }
            if supplied.script_pubkey != bridge_script {
                return Ok(false);
            }
        }

        let rebuilt = self
            .build_bridge_txs(
                candidate.utxos.clone(),
                requested_outputs,
                config,
                candidate.fee_rate,
            )
            .await?;
        Ok(rebuilt.as_slice() == std::slice::from_ref(candidate))
    }

    /// Generates taproot signature hashes for all inputs
    #[instrument(skip(self, unsigned_tx), target = "bitcoin_transaction_builder")]
    pub fn get_tr_sighashes(&self, unsigned_tx: &UnsignedBridgeTx) -> Result<Vec<Vec<u8>>> {
        let mut sighash_cache = SighashCache::new(&unsigned_tx.tx);
        let sighash_type = TapSighashType::All;

        let txout_list: Vec<TxOut> = unsigned_tx
            .utxos
            .iter()
            .map(|(_, txout)| txout.clone())
            .collect();

        let mut sighashes = Vec::new();
        for (i, _) in txout_list.iter().enumerate() {
            let sighash = sighash_cache
                .taproot_key_spend_signature_hash(i, &Prevouts::All(&txout_list), sighash_type)
                .context("Error taproot_key_spend_signature_hash")?;
            sighashes.push(sighash.to_raw_hash().to_byte_array().to_vec());
        }

        Ok(sighashes)
    }

    /// Creates an OP_RETURN script with prefix and data
    pub fn create_op_return_script(prefix: &[u8], inputs: Vec<Vec<u8>>) -> Result<ScriptBuf> {
        let mut encoded_data =
            PushBytesBuf::with_capacity(prefix.len() + inputs.iter().map(Vec::len).sum::<usize>());
        encoded_data
            .extend_from_slice(prefix)
            .context("Failed to encode OP_RETURN prefix")?;
        for (index, input) in inputs.into_iter().enumerate() {
            encoded_data
                .extend_from_slice(&input)
                .with_context(|| format!("Failed to encode OP_RETURN input {index}"))?;
        }
        anyhow::ensure!(
            encoded_data.len() <= 80,
            "Invalid OP_RETURN data size {}",
            encoded_data.len()
        );
        Ok(ScriptBuf::new_op_return(encoded_data))
    }

    // Private helper methods

    async fn get_available_utxos(
        &self,
        config: &TransactionBuilderConfig,
    ) -> Result<Vec<(OutPoint, TxOut)>> {
        if let Some(ref available_utxos) = config.default_available_utxos_opt {
            Ok(available_utxos.clone())
        } else {
            self.utxo_manager
                .get_available_utxos(config.bridge_address.clone())
                .await
        }
    }

    async fn get_fee_rate(&self, config: &TransactionBuilderConfig) -> Result<u64> {
        if let Some(fee_rate) = config.default_fee_rate_opt {
            Ok(fee_rate)
        } else {
            let network_fee = self.utxo_manager.get_btc_client().get_fee_rate(1).await?;
            Ok(std::cmp::max(network_fee, 1))
        }
    }

    async fn build_single_bridge_tx(
        &self,
        output_chunk: &[TransactionOutput],
        utxos_pool: &mut Vec<(OutPoint, TxOut)>,
        config: &TransactionBuilderConfig,
        fee_rate: u64,
        tx_index: usize,
    ) -> Result<Option<UnsignedBridgeTx>> {
        let (tx_fee, selected_utxos) = self.prepare_build_transaction(
            output_chunk.to_vec(),
            utxos_pool,
            fee_rate,
            config.fee_strategy.clone(),
        )?;

        // Fee-ineligible chunks leave the UTXO pool untouched for later chunks.
        if tx_fee.outputs_with_fees.is_empty() {
            return Ok(None);
        }

        anyhow::ensure!(
            self.estimate_transaction_weight(
                selected_utxos.len() as u64,
                tx_fee.outputs_with_fees.len() as u64,
            ) <= config.max_tx_weight,
            "Transaction with {} outputs exceeds weight limit",
            tx_fee.outputs_with_fees.len()
        );
        drop(utxos_pool.drain(..selected_utxos.len()));

        let change = self.calculate_change(&tx_fee, &selected_utxos, tx_index)?;
        let unsigned_tx = self.build_transaction(&tx_fee, &selected_utxos, config, change)?;
        let txid = unsigned_tx.compute_txid();

        if change != Amount::ZERO {
            let change_vout = unsigned_tx.output.len() - 1;
            let change_utxo = (
                OutPoint::new(txid, change_vout as u32),
                unsigned_tx.output[change_vout].clone(),
            );
            let insert_at = utxos_pool.partition_point(|(_, output)| output.value >= change);
            utxos_pool.insert(insert_at, change_utxo);
        }

        Ok(Some(UnsignedBridgeTx {
            tx: unsigned_tx,
            txid,
            utxos: selected_utxos,
            change_amount: change,
            fee_rate,
            fee: tx_fee.fee,
        }))
    }

    fn calculate_change(
        &self,
        tx_fee: &TransactionWithFee,
        selected_utxos: &[(OutPoint, TxOut)],
        tx_index: usize,
    ) -> Result<Amount> {
        let total_input = selected_utxos
            .iter()
            .try_fold(Amount::ZERO, |sum, (_, output)| {
                sum.checked_add(output.value)
            })
            .with_context(|| format!("Input amount overflow in tx index {tx_index}"))?;
        let total_needed = tx_fee
            .total_value_needed
            .checked_add(tx_fee.fee)
            .with_context(|| format!("Total amount overflow in tx index {tx_index}"))?;
        anyhow::ensure!(
            total_input >= total_needed,
            "Insufficient funds in tx index {tx_index}: have {total_input}, need {total_needed}"
        );
        total_input
            .checked_sub(total_needed)
            .context("Change amount calculation overflow")
    }

    fn build_transaction(
        &self,
        tx_fee: &TransactionWithFee,
        selected_utxos: &[(OutPoint, TxOut)],
        config: &TransactionBuilderConfig,
        change_amount: Amount,
    ) -> Result<Transaction> {
        let input = selected_utxos
            .iter()
            .map(|(outpoint, _)| TxIn {
                previous_output: *outpoint,
                script_sig: ScriptBuf::default(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(),
            })
            .collect();
        let mut output: Vec<_> = tx_fee
            .outputs_with_fees
            .iter()
            .map(|output| output.output.clone())
            .collect();
        let op_return_data = config.op_return_data_input_opt.clone().map_or_else(
            || {
                tx_fee
                    .outputs_with_fees
                    .iter()
                    .filter_map(|output| output.op_return_data.clone())
                    .collect()
            },
            |data| vec![data],
        );
        output.push(TxOut {
            value: Amount::ZERO,
            script_pubkey: Self::create_op_return_script(&config.op_return_prefix, op_return_data)?,
        });
        if change_amount > Amount::ZERO {
            output.push(TxOut {
                value: change_amount,
                script_pubkey: config.bridge_address.script_pubkey(),
            });
        }

        Ok(Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input,
            output,
        })
    }
}
