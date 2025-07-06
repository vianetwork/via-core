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
use tracing::{debug, instrument};
use via_btc_client::traits::BitcoinOps;
use via_verifier_types::transaction::UnsignedBridgeTx;

use crate::{fee::FeeStrategy, utxo_manager::UtxoManager};

#[derive(Debug, Clone)]
pub struct TransactionBuilder {
    pub utxo_manager: UtxoManager,
    bridge_address: Address,
}

impl TransactionBuilder {
    #[instrument(skip(btc_client), target = "bitcoin_transaction_builder")]
    pub fn new(btc_client: Arc<dyn BitcoinOps>, bridge_address: Address) -> Result<Self> {
        let utxo_manager = UtxoManager::new(
            btc_client.clone(),
            bridge_address.clone(),
            Amount::from_sat(1000),
            128,
        );

        Ok(Self {
            utxo_manager,
            bridge_address,
        })
    }

    #[instrument(
        skip(self, outputs, op_return_data, fee_strategy),
        target = "bitcoin_transaction_builder"
    )]
    pub async fn build_transaction_with_op_return(
        &self,
        outputs: Vec<TxOut>,
        op_return_prefix: &[u8],
        op_return_data: Vec<[u8; 32]>,
        fee_strategy: Arc<dyn FeeStrategy>,
        default_fee_rate: Option<u64>,
    ) -> Result<UnsignedBridgeTx> {
        self.utxo_manager.sync_context_with_blockchain().await?;

        // Get available UTXOs first to estimate number of inputs
        let available_utxos = self.utxo_manager.get_available_utxos().await?;

        // Get fee rate
        let fee_rate = if let Some(fee_rate) = default_fee_rate {
            fee_rate
        } else {
            std::cmp::max(self.utxo_manager.get_btc_client().get_fee_rate(1).await?, 1)
        };

        // Create OP_RETURN output with proof txid
        let op_return_data =
            TransactionBuilder::create_op_return_script(op_return_prefix, op_return_data)?;

        let op_return_output = TxOut {
            value: Amount::ZERO,
            script_pubkey: op_return_data,
        };

        let (mut adjusted_outputs, adjusted_total_value_needed, actual_fee, selected_utxos) = self
            .prepare_build_transaction(outputs, &available_utxos, fee_rate, fee_strategy)
            .await?;

        // Calculate total input amount
        let total_input_amount: Amount = selected_utxos
            .iter()
            .try_fold(Amount::ZERO, |acc, (_, txout)| acc.checked_add(txout.value))
            .ok_or_else(|| anyhow::anyhow!("Input amount overflow"))?;

        // Verify we have enough funds with actual fee
        let total_needed = adjusted_total_value_needed
            .checked_add(actual_fee)
            .ok_or_else(|| anyhow::anyhow!("Total amount overflow"))?;

        if total_input_amount < total_needed {
            return Err(anyhow::anyhow!(
                "Insufficient funds: have {}, need {}",
                total_input_amount,
                total_needed
            ));
        }

        // Create inputs
        let inputs: Vec<TxIn> = selected_utxos
            .iter()
            .map(|(outpoint, _)| TxIn {
                previous_output: *outpoint,
                script_sig: ScriptBuf::default(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(),
            })
            .collect();

        // Add OP_RETURN output
        adjusted_outputs.push(op_return_output);

        // Add change output if needed
        let change_amount = total_input_amount
            .checked_sub(total_needed)
            .ok_or_else(|| anyhow::anyhow!("Change amount calculation overflow"))?;

        if change_amount.to_sat() > 0 {
            adjusted_outputs.push(TxOut {
                value: change_amount,
                script_pubkey: self.bridge_address.script_pubkey(),
            });
        }

        // Create unsigned transaction
        let unsigned_tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs,
            output: adjusted_outputs.clone(),
        };

        let txid = unsigned_tx.compute_txid();

        let bridge_tx = UnsignedBridgeTx {
            tx: unsigned_tx.clone(),
            txid,
            utxos: selected_utxos,
            change_amount,
            fee_rate,
            fee: actual_fee,
        };

        // When there are no withdrawal to process due to low value requested by a user. The verifier network will not broadcast it to network,
        // in this case no need to insert it inside the utxo manager.
        if !bridge_tx.is_empty() {
            self.utxo_manager.insert_transaction(unsigned_tx).await;
        }

        debug!("Unsigned created successfully");

        Ok(bridge_tx)
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
            .apply_fee_to_inputs(
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
    pub fn create_op_return_script(prefix: &[u8], inputs: Vec<[u8; 32]>) -> Result<ScriptBuf> {
        let mut data = Vec::with_capacity(prefix.len() + 32);
        data.extend_from_slice(prefix);
        for input in inputs {
            data.extend_from_slice(&input);
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
