use std::sync::Arc;

use anyhow::{Context, Result};
use bitcoin::{
    absolute,
    hashes::Hash,
    script::PushBytesBuf,
    sighash::{Prevouts, SighashCache},
    transaction, Address, Amount, OutPoint, ScriptBuf, Sequence, TapSighashType, Transaction, TxIn,
    TxOut, Witness,
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
    types::TransactionMetadata,
    utxo_manager::UtxoManager,
};

#[derive(Debug, Clone)]
pub struct TransactionBuilder {
    pub utxo_manager: UtxoManager,
}

impl TransactionBuilder {
    #[instrument(skip(btc_client), target = "bitcoin_transaction_builder")]
    pub fn new(btc_client: Arc<dyn BitcoinOps>) -> Result<Self> {
        let utxo_manager = UtxoManager::new(btc_client.clone(), Amount::from_sat(1000), 128);

        Ok(Self { utxo_manager })
    }

    #[instrument(
        skip(self, outputs, op_return_data, fee_strategy),
        target = "bitcoin_transaction_builder"
    )]
    pub async fn build_transaction_with_op_return(
        &self,
        outputs: Vec<TxOut>,
        op_return_prefix: &[u8],
        op_return_data: Vec<&[u8]>,
        fee_strategy: Arc<dyn FeeStrategy>,
        default_fee_rate: Option<u64>,
        default_available_utxos: Option<Vec<(OutPoint, TxOut)>>,
        max_tx_weight: u64,
        bridge_address: Address,
    ) -> Result<Vec<UnsignedBridgeTx>> {
        self.utxo_manager.sync_context_with_blockchain().await?;

        // Get available UTXOs first to estimate number of inputs
        let available_utxos = if let Some(available_utxos) = default_available_utxos {
            available_utxos
        } else {
            let available_utxos = self
                .utxo_manager
                .get_available_utxos(bridge_address.clone())
                .await?;
            available_utxos
        };

        // Get fee rate
        let fee_rate = if let Some(fee_rate) = default_fee_rate {
            fee_rate
        } else {
            std::cmp::max(self.utxo_manager.get_btc_client().get_fee_rate(1).await?, 1)
        };

        let txs_metadata = self
            .get_transaction_metadata(
                &available_utxos,
                &outputs,
                fee_rate,
                fee_strategy.clone(),
                max_tx_weight,
            )
            .await?;
        let bridge_txs = self.build_bridge_txs(
            &txs_metadata,
            fee_rate,
            op_return_prefix,
            op_return_data,
            bridge_address,
        )?;

        Ok(bridge_txs)
    }

    pub async fn utxo_manager_insert_transaction(&self, tx: Transaction) {
        self.utxo_manager.insert_transaction(tx).await;
    }

    pub(crate) fn estimate_transaction_weight(&self, inputs: u64, outputs: u64) -> u64 {
        // Base size calculation (gets 4x weight)
        let base_size = TX_OVERHEAD
            + INPUT_BASE_SIZE * inputs
            + OUTPUT_SIZE * (outputs + 1) // include change output
            + OP_RETURN_SIZE;

        // Witness size calculation (gets 1x weight)
        let witness_size = if inputs > 0 {
            WITNESS_OVERHEAD + INPUT_WITNESS_SIZE * inputs
        } else {
            0
        };

        // Weight = (base_size × 4) + (witness_size × 1)
        base_size * 4 + witness_size
    }

    pub(crate) fn chunk_outputs(outputs: &[TxOut], chunks: usize) -> Vec<Vec<TxOut>> {
        let mut result = Vec::new();
        let mut start = 0;
        for i in 0..chunks {
            let remaining = outputs.len() - start;
            let chunk_size = (remaining + (chunks - i - 1)) / (chunks - i);
            let end = start + chunk_size;
            result.push(outputs[start..end].to_vec());
            start = end;
        }
        result
    }

    pub(crate) async fn get_transaction_metadata(
        &self,
        available_utxos: &[(OutPoint, TxOut)],
        outputs: &[TxOut],
        fee_rate: u64,
        fee_strategy: Arc<dyn FeeStrategy>,
        max_tx_weight: u64,
    ) -> anyhow::Result<Vec<TransactionMetadata>> {
        // Try chunk sizes from 1 to outputs.len()
        for chunks in 1..=outputs.len() {
            let output_chunks = TransactionBuilder::chunk_outputs(&outputs, chunks);
            let mut utxos_pool = available_utxos.to_vec();
            let mut result = Vec::new();
            let mut all_chunks_fit = true;

            for output_chunk in output_chunks {
                let (adjusted_outputs, adjusted_total_value_needed, actual_fee, selected_utxos) =
                    self.prepare_build_transaction(
                        output_chunk.clone(),
                        &utxos_pool,
                        fee_rate,
                        fee_strategy.clone(),
                    )
                    .await?;

                let tx_weight = self.estimate_transaction_weight(
                    selected_utxos.len() as u64,
                    adjusted_outputs.len() as u64,
                );

                if tx_weight > max_tx_weight as u64 {
                    all_chunks_fit = false;
                    break;
                }

                // Remove used UTXOs from the pool
                utxos_pool.retain(|(outpoint, _)| {
                    !selected_utxos.iter().any(|(used, _)| used == outpoint)
                });

                result.push(TransactionMetadata {
                    outputs: adjusted_outputs,
                    total_amount: adjusted_total_value_needed,
                    fee: actual_fee,
                    inputs: selected_utxos,
                });
            }

            if all_chunks_fit {
                return Ok(result);
            }
        }
        anyhow::bail!("Unable to build transactions within standard weight limits")
    }

    pub fn build_bridge_txs(
        &self,
        txs_metadata: &[TransactionMetadata],
        fee_rate: u64,
        op_return_prefix: &[u8],
        op_return_data_input: Vec<&[u8]>,
        bridge_address: Address,
    ) -> anyhow::Result<Vec<UnsignedBridgeTx>> {
        let mut bridge_txs = vec![];

        for (i, tx_metadata) in txs_metadata.iter().enumerate() {
            // Calculate total input amount
            let total_input_amount: Amount = tx_metadata
                .inputs
                .iter()
                .try_fold(Amount::ZERO, |acc, (_, txout)| acc.checked_add(txout.value))
                .ok_or_else(|| anyhow::anyhow!("Input amount overflow in tx index {}", i))?;

            // Calculate total needed (outputs + fee)
            let total_needed = tx_metadata
                .total_amount
                .checked_add(tx_metadata.fee)
                .ok_or_else(|| anyhow::anyhow!("Total amount overflow in tx index {}", i))?;

            // Verify we have enough input
            if total_input_amount < total_needed {
                return Err(anyhow::anyhow!(
                    "Insufficient funds in tx index {}: have {}, need {}",
                    i,
                    total_input_amount,
                    total_needed
                ));
            }

            // Create inputs
            let inputs: Vec<TxIn> = tx_metadata
                .inputs
                .iter()
                .map(|(outpoint, _)| TxIn {
                    previous_output: *outpoint,
                    script_sig: ScriptBuf::default(),
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: Witness::default(),
                })
                .collect();

            let mut op_return_data = op_return_data_input.clone();
            let i_bytes = (i as u64).to_le_bytes().to_vec();
            op_return_data.push(&i_bytes);

            // Create OP_RETURN output with proof txid
            let op_return_data =
                TransactionBuilder::create_op_return_script(op_return_prefix, op_return_data)?;

            let op_return_output = TxOut {
                value: Amount::ZERO,
                script_pubkey: op_return_data,
            };

            // Construct outputs: existing + OP_RETURN + optional change
            let mut outputs = tx_metadata.outputs.clone();
            outputs.push(op_return_output.clone());

            let change_amount = total_input_amount
                .checked_sub(total_needed)
                .ok_or_else(|| {
                    anyhow::anyhow!("Change amount calculation overflow in tx index {}", i)
                })?;

            if change_amount.to_sat() > 0 {
                outputs.push(TxOut {
                    value: change_amount,
                    script_pubkey: bridge_address.script_pubkey(),
                });
            }

            // Build unsigned transaction
            let unsigned_tx = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: inputs,
                output: outputs.clone(),
            };

            let txid = unsigned_tx.compute_txid();

            let bridge_tx = UnsignedBridgeTx {
                tx: unsigned_tx.clone(),
                txid,
                utxos: tx_metadata.inputs.clone(),
                change_amount,
                fee_rate,
                fee: tx_metadata.fee,
            };

            bridge_txs.push(bridge_tx);
        }

        Ok(bridge_txs)
    }

    pub async fn prepare_build_transaction(
        &self,
        outputs: Vec<TxOut>,
        available_utxos: &[(OutPoint, TxOut)],
        fee_rate: u64,
        fee_strategy: Arc<dyn FeeStrategy>,
    ) -> anyhow::Result<(Vec<TxOut>, Amount, Amount, Vec<(OutPoint, TxOut)>)> {
        let adjusted_outputs = outputs;

        let total_needed = adjusted_outputs
            .iter()
            .map(|output| output.value)
            .sum::<Amount>();

        // Select UTXOs for the total amount including fee
        let selected_utxos = self
            .utxo_manager
            .select_utxos_by_target_value(&available_utxos, total_needed)
            .await?;

        tracing::debug!("Selected UTXOs {:?}", &selected_utxos);

        // Apply the fee to the outputs.
        let (new_adjusted_outputs, fee, adjusted_total_value_needed) = fee_strategy
            .apply_fee_to_outputs(
                adjusted_outputs.clone(),
                selected_utxos.len() as u32,
                fee_rate,
            )?;

        if fee == Amount::ZERO {
            anyhow::bail!("Error to prepare build transaction, fee=0");
        }

        return Ok((
            new_adjusted_outputs,
            adjusted_total_value_needed,
            fee,
            selected_utxos,
        ));
    }

    #[instrument(skip(self, unsigned_tx), target = "bitcoin_transaction_builder")]
    pub fn get_tr_sighashes(&self, unsigned_tx: &UnsignedBridgeTx) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut sighash_cache = SighashCache::new(&unsigned_tx.tx);
        let sighash_type = TapSighashType::All;

        let txout_list: Vec<TxOut> = unsigned_tx
            .utxos
            .iter()
            .map(|(_, txout)| txout.clone())
            .collect();

        let mut sighashes = vec![];
        for (i, _) in txout_list.iter().enumerate() {
            let sighash = sighash_cache
                .taproot_key_spend_signature_hash(i, &Prevouts::All(&txout_list), sighash_type)
                .with_context(|| "Error taproot_key_spend_signature_hash")?;
            sighashes.push(sighash.to_raw_hash().to_byte_array().to_vec());
        }

        Ok(sighashes)
    }

    // Helper function to create OP_RETURN script
    pub fn create_op_return_script(prefix: &[u8], inputs: Vec<&[u8]>) -> Result<ScriptBuf> {
        let total_input_size: usize = inputs.iter().map(|input| input.len()).sum();
        let mut data = Vec::with_capacity(prefix.len() + total_input_size);

        data.extend_from_slice(prefix);
        for input in inputs {
            data.extend_from_slice(input);
        }

        let mut encoded_data = PushBytesBuf::with_capacity(data.len());
        encoded_data.extend_from_slice(&data).ok();

        Ok(ScriptBuf::new_op_return(encoded_data))
    }

    #[instrument(skip(self), target = "bitcoin_transaction_builder")]
    fn estimate_fee(&self, input_count: u32, output_count: u32, fee_rate: u64) -> Result<Amount> {
        // Estimate transaction size
        let base_size = 10_u64; // version + locktime
        let input_size = 148_u64 * u64::from(input_count); // approximate size per input
        let output_size = 34_u64 * u64::from(output_count); // approximate size per output

        let total_size = base_size + input_size + output_size;
        let fee = fee_rate * total_size;

        Ok(Amount::from_sat(fee))
    }

    #[instrument(skip(self, utxos), target = "bitcoin_transaction_builder")]
    fn estimate_input_count(
        &self,
        utxos: &[(OutPoint, TxOut)],
        target_amount: Amount,
    ) -> Result<u32> {
        let mut count: u32 = 0;
        let mut total = Amount::ZERO;

        for utxo in utxos {
            count += 1;
            total = total
                .checked_add(utxo.1.value)
                .ok_or_else(|| anyhow::anyhow!("Amount overflow during input count estimation"))?;

            if total >= target_amount {
                break;
            }
        }
        // Add one more to our estimate to be safe
        Ok(count.saturating_add(1))
    }
}
