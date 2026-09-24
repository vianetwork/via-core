use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

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
use via_verifier_types::{
    transaction::UnsignedBridgeTx, withdrawal::WithdrawalRequest,
    withdrawal_observation::WithdrawalObservation,
};

use crate::{
    constants::{
        INPUT_BASE_SIZE, INPUT_WITNESS_SIZE, OP_RETURN_SIZE, OUTPUT_SIZE, TX_OVERHEAD,
        WITNESS_OVERHEAD,
    },
    fee::{fee_size, FeeStrategy, WithdrawalFeeStrategy},
    types::{TransactionBuilderConfig, TransactionOutput, TransactionWithFee},
    utxo_manager::UtxoManager,
};

/// Helper struct to hold calculated transaction amounts
pub struct TransactionAmounts {
    pub total_input: Amount,
    pub total_needed: Amount,
    pub change: Amount,
}

/// Helper struct to hold transaction components during construction
pub struct TransactionComponents {
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
    pub change_utxo: TxOut,
}

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
        excluded_inputs: &[OutPoint],
    ) -> Result<Vec<UnsignedBridgeTx>> {
        self.utxo_manager.sync_context_with_blockchain().await?;

        let mut available_utxos = self.get_available_utxos(&config).await?;
        if !excluded_inputs.is_empty() {
            let excluded: HashSet<_> = excluded_inputs.iter().collect();
            available_utxos.retain(|(outpoint, _)| !excluded.contains(outpoint));
        }
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
            "Output chunk size must be positive"
        );
        let output_chunks = self.chunk_outputs(&outputs, config.max_output_per_tx);
        let mut utxos_pool = available_utxos;
        let mut bridge_txs = Vec::new();

        for (i, output_chunk) in output_chunks.iter().enumerate() {
            let bridge_tx = self
                .build_single_bridge_tx(output_chunk, &mut utxos_pool, &config, fee_rate, i)
                .await?;

            bridge_txs.push(bridge_tx);
        }

        Ok(bridge_txs)
    }

    /// Prepares transaction by selecting UTXOs and calculating fees
    pub async fn prepare_build_transaction(
        &self,
        outputs: Vec<TransactionOutput>,
        available_utxos: &[(OutPoint, TxOut)],
        fee_rate: u64,
        fee_strategy: Arc<dyn FeeStrategy>,
    ) -> Result<(TransactionWithFee, Vec<(OutPoint, TxOut)>)> {
        let total_needed = self.calculate_total_output_value(&outputs)?;
        let selected_utxos = self.select_utxos(available_utxos, total_needed).await?;

        tracing::debug!("Selected UTXOs {:?}", &selected_utxos);

        let tx_fee = fee_strategy.apply_fee_to_outputs(
            outputs,
            u32::try_from(selected_utxos.len())?,
            fee_rate,
        )?;

        if tx_fee.fee == Amount::ZERO {
            anyhow::bail!("Error to prepare build transaction, fee=0");
        }

        Ok((tx_fee, selected_utxos))
    }

    /// Reconstructs exactly one ordered proposal; never selects coins or drops requests.
    /// Prefix sufficiency preserves utxo_manager's ordered selector at 8a49f355.
    pub fn build_fixed_bridge_tx(
        &self,
        inputs: &[(OutPoint, TxOut)],
        outputs: Vec<TransactionOutput>,
        config: &TransactionBuilderConfig,
        fee_rate: u64,
    ) -> Result<UnsignedBridgeTx> {
        anyhow::ensure!(
            !inputs.is_empty() && !outputs.is_empty(),
            "Empty fixed withdrawal"
        );
        anyhow::ensure!(
            outputs.len() <= config.max_output_per_tx,
            "Too many withdrawal outputs"
        );
        let gross = self.calculate_total_output_value(&outputs)?;
        let bridge_script = config.bridge_address.script_pubkey();
        let mut seen = HashSet::with_capacity(inputs.len());
        let mut total = Amount::ZERO;
        for (index, (outpoint, prevout)) in inputs.iter().enumerate() {
            anyhow::ensure!(seen.insert(*outpoint), "Duplicate withdrawal input");
            anyhow::ensure!(
                prevout.script_pubkey == bridge_script,
                "Non-bridge withdrawal input"
            );
            total = total
                .checked_add(prevout.value)
                .context("Withdrawal input amount overflow")?;
            anyhow::ensure!(
                index + 1 == inputs.len() || total < gross,
                "Redundant trailing withdrawal inputs"
            );
        }
        anyhow::ensure!(total >= gross, "Insufficient fixed withdrawal inputs");
        let output_count = outputs.len();
        let tx_fee = config.fee_strategy.apply_fee_to_outputs(
            outputs,
            u32::try_from(inputs.len())?,
            fee_rate,
        )?;
        anyhow::ensure!(tx_fee.fee != Amount::ZERO, "Zero withdrawal fee");
        anyhow::ensure!(
            tx_fee.outputs_with_fees.len() == output_count,
            "Fee would drop a withdrawal"
        );
        anyhow::ensure!(
            self.calculate_total_needed(&tx_fee, 0)? == gross,
            "Fee strategy changes gross withdrawal value"
        );
        self.validate_transaction_weight(&tx_fee, inputs, config.max_tx_weight)?;
        let amounts = self.calculate_and_validate_amounts(&tx_fee, inputs, 0)?;
        let components =
            self.build_transaction_components(&tx_fee, inputs, config, amounts.change)?;
        let tx = self.assemble_transaction(components.inputs, components.outputs);
        Ok(UnsignedBridgeTx {
            txid: tx.compute_txid(),
            tx,
            utxos: inputs.to_vec(),
            change_amount: amounts.change,
            fee: tx_fee.fee,
            fee_rate,
        })
    }

    /// Parent bytes prove the referenced output, not current unspentness (BIP341 prevouts).
    pub fn verify_fixed_bridge_tx(
        &self,
        candidate: &UnsignedBridgeTx,
        requested_outputs: Vec<TransactionOutput>,
        config: &TransactionBuilderConfig,
        parents: &[Transaction],
    ) -> Result<()> {
        Self::validate_input_alignment(&candidate.tx, &candidate.utxos)?;
        let rebuilt = self.build_fixed_bridge_tx(
            &candidate.utxos,
            requested_outputs,
            config,
            candidate.fee_rate,
        )?;
        anyhow::ensure!(
            &rebuilt == candidate,
            "Withdrawal differs from exact authorized construction"
        );
        let parents: HashMap<_, _> = parents.iter().map(|tx| (tx.compute_txid(), tx)).collect();
        for (outpoint, supplied) in &candidate.utxos {
            let parent = parents
                .get(&outpoint.txid)
                .context("Missing verified withdrawal parent")?;
            anyhow::ensure!(
                parent.output.get(outpoint.vout as usize) == Some(supplied),
                "Withdrawal prevout differs from parent bytes"
            );
        }
        Ok(())
    }

    /// Validates independent payment evidence; the caller authenticates parent bytes and inclusion.
    /// Only witness is excluded: scriptSig, sequences and every output remain exact.
    pub fn verify_observed_withdrawal(
        &self,
        observation: &WithdrawalObservation,
        requests: &[WithdrawalRequest],
        config: &TransactionBuilderConfig,
    ) -> Result<()> {
        anyhow::ensure!(!requests.is_empty(), "Empty observed withdrawal");
        anyhow::ensure!(
            observation.withdrawals.len() == requests.len(),
            "Observed request count mismatch"
        );
        Self::validate_input_alignment(&observation.transaction, &observation.prevouts)?;
        let input_total = self.calculate_total_input_amount(&observation.prevouts, 0)?;
        let output_total =
            observation
                .transaction
                .output
                .iter()
                .try_fold(Amount::ZERO, |sum, out| {
                    sum.checked_add(out.value)
                        .context("Observed output sum overflow")
                })?;
        let fee = input_total
            .checked_sub(output_total)
            .context("Observed negative fee")?
            .to_sat();
        let n = u64::try_from(requests.len())?;
        anyhow::ensure!(
            fee > 0 && fee % n == 0,
            "Observed fee cannot be split equally"
        );
        let size = fee_size(
            u32::try_from(observation.prevouts.len())?,
            u32::try_from(requests.len())?,
        );
        // Existing economics: F = ceil(rate * size / n) * n, not actual witness vsize.
        let min_rate = (fee - n) / size + 1;
        let max_rate = fee / size;
        anyhow::ensure!(
            min_rate == max_rate && min_rate > 0,
            "Observed fee rate interpretation is impossible or ambiguous"
        );
        let canonical_fee = WithdrawalFeeStrategy::new().estimate_fee(
            u32::try_from(observation.prevouts.len())?,
            u32::try_from(requests.len())?,
            min_rate,
        )?;
        anyhow::ensure!(
            canonical_fee.to_sat() == fee,
            "Observed fee equation mismatch"
        );
        let mut references = HashSet::with_capacity(requests.len());
        let mut outputs = Vec::with_capacity(requests.len());
        for (index, (request, observed)) in
            requests.iter().zip(&observation.withdrawals).enumerate()
        {
            let reference = hex::decode(&request.id)?;
            anyhow::ensure!(
                reference.len() == 10 && references.insert(reference.clone()),
                "Invalid or duplicate withdrawal reference"
            );
            let actual = observation
                .transaction
                .output
                .get(index)
                .context("Missing observed payment")?;
            anyhow::ensure!(
                observed.vout == u32::try_from(index)?
                    && observed.reference == request.id
                    && observed.script_pubkey == actual.script_pubkey
                    && observed.amount == actual.value,
                "Observed payment identity mismatch"
            );
            outputs.push(TransactionOutput {
                output: TxOut {
                    value: request.amount,
                    script_pubkey: request.receiver.script_pubkey(),
                },
                op_return_data: Some(reference),
            });
        }
        anyhow::ensure!(
            config.op_return_data_input_opt.is_none(),
            "Withdrawal metadata override is not authorized"
        );
        let rebuilt =
            self.build_fixed_bridge_tx(&observation.prevouts, outputs, config, min_rate)?;
        anyhow::ensure!(
            rebuilt.fee.to_sat() == fee,
            "Observed fee strategy mismatch"
        );
        let mut unsigned = observation.transaction.clone();
        for input in &mut unsigned.input {
            input.witness = Witness::default();
        }
        anyhow::ensure!(
            rebuilt.tx == unsigned,
            "Observed payment differs from exact authorized construction"
        );
        Ok(())
    }

    fn validate_input_alignment(tx: &Transaction, prevouts: &[(OutPoint, TxOut)]) -> Result<()> {
        anyhow::ensure!(
            !tx.input.is_empty() && tx.input.len() == prevouts.len(),
            "Input/prevout count mismatch"
        );
        let mut seen = HashSet::with_capacity(prevouts.len());
        for (input, (outpoint, _)) in tx.input.iter().zip(prevouts) {
            anyhow::ensure!(
                input.previous_output == *outpoint,
                "Input/prevout order mismatch"
            );
            anyhow::ensure!(seen.insert(*outpoint), "Duplicate transaction input");
        }
        Ok(())
    }

    /// Generates taproot signature hashes for all inputs
    #[instrument(skip(self, unsigned_tx), target = "bitcoin_transaction_builder")]
    pub fn get_tr_sighashes(&self, unsigned_tx: &UnsignedBridgeTx) -> Result<Vec<Vec<u8>>> {
        Self::validate_input_alignment(&unsigned_tx.tx, &unsigned_tx.utxos)?;
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
        let data = Self::concatenate_op_return_data(prefix, inputs);
        let encoded_data = Self::encode_data(&data)?;
        let op_return = ScriptBuf::new_op_return(encoded_data);

        Self::validate_op_return_size(&op_return)?;
        Ok(op_return)
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

    fn chunk_outputs(
        &self,
        outputs: &[TransactionOutput],
        max_per_tx: usize,
    ) -> Vec<Vec<TransactionOutput>> {
        outputs
            .chunks(max_per_tx)
            .map(|chunk| chunk.to_vec())
            .collect()
    }

    async fn build_single_bridge_tx(
        &self,
        output_chunk: &[TransactionOutput],
        utxos_pool: &mut Vec<(OutPoint, TxOut)>,
        config: &TransactionBuilderConfig,
        fee_rate: u64,
        tx_index: usize,
    ) -> Result<UnsignedBridgeTx> {
        // Step 1: Prepare transaction - select UTXOs and calculate fees
        let (tx_fee, selected_utxos) = self
            .prepare_transaction_with_utxos(output_chunk, utxos_pool, fee_rate, config)
            .await?;

        // Step 2: Validate and update UTXO pool
        self.validate_and_update_utxos(&tx_fee, &selected_utxos, utxos_pool, config.max_tx_weight)?;

        // Step 3: Calculate amounts and validate funds
        let amounts = self.calculate_and_validate_amounts(&tx_fee, &selected_utxos, tx_index)?;

        // Step 4: Build transaction components
        let tx_components =
            self.build_transaction_components(&tx_fee, &selected_utxos, config, amounts.change)?;

        // Step 5: Assemble and finalize transaction
        let unsigned_tx =
            self.assemble_transaction(tx_components.inputs, tx_components.outputs.clone());
        let txid = unsigned_tx.compute_txid();

        // Step 6: Update pool with change UTXO for potential chaining
        self.add_change_to_pool(
            utxos_pool,
            txid,
            &tx_components.outputs,
            tx_components.change_utxo,
        );

        Ok(UnsignedBridgeTx {
            tx: unsigned_tx,
            txid,
            utxos: selected_utxos,
            change_amount: amounts.change,
            fee_rate,
            fee: tx_fee.fee,
        })
    }

    async fn prepare_transaction_with_utxos(
        &self,
        output_chunk: &[TransactionOutput],
        utxos_pool: &[(OutPoint, TxOut)],
        fee_rate: u64,
        config: &TransactionBuilderConfig,
    ) -> Result<(TransactionWithFee, Vec<(OutPoint, TxOut)>)> {
        self.prepare_build_transaction(
            output_chunk.to_vec(),
            utxos_pool,
            fee_rate,
            config.fee_strategy.clone(),
        )
        .await
    }

    fn validate_and_update_utxos(
        &self,
        tx_fee: &TransactionWithFee,
        selected_utxos: &[(OutPoint, TxOut)],
        utxos_pool: &mut Vec<(OutPoint, TxOut)>,
        max_tx_weight: u64,
    ) -> Result<()> {
        self.validate_transaction_weight(tx_fee, selected_utxos, max_tx_weight)?;
        self.remove_used_utxos(utxos_pool, selected_utxos);
        Ok(())
    }

    fn calculate_and_validate_amounts(
        &self,
        tx_fee: &TransactionWithFee,
        selected_utxos: &[(OutPoint, TxOut)],
        tx_index: usize,
    ) -> Result<TransactionAmounts> {
        let total_input = self.calculate_total_input_amount(selected_utxos, tx_index)?;
        let total_needed = self.calculate_total_needed(tx_fee, tx_index)?;

        self.validate_sufficient_funds(total_input, total_needed, tx_index)?;

        let change = total_input
            .checked_sub(total_needed)
            .context("Change amount calculation overflow")?;

        Ok(TransactionAmounts {
            total_input,
            total_needed,
            change,
        })
    }

    fn build_transaction_components(
        &self,
        tx_fee: &TransactionWithFee,
        selected_utxos: &[(OutPoint, TxOut)],
        config: &TransactionBuilderConfig,
        change_amount: Amount,
    ) -> Result<TransactionComponents> {
        let inputs = self.create_inputs(selected_utxos);
        let op_return_output = self.create_op_return_output(tx_fee, config)?;
        let change_utxo = self.create_change_output(change_amount, config);

        let mut outputs = self.create_transaction_outputs(tx_fee);
        outputs.push(op_return_output);

        if change_utxo.value > Amount::ZERO {
            outputs.push(change_utxo.clone());
        }

        Ok(TransactionComponents {
            inputs,
            outputs,
            change_utxo,
        })
    }

    fn assemble_transaction(&self, inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Transaction {
        self.build_unsigned_transaction(inputs, outputs)
    }

    fn calculate_total_output_value(&self, outputs: &[TransactionOutput]) -> Result<Amount> {
        outputs.iter().try_fold(Amount::ZERO, |sum, output| {
            sum.checked_add(output.output.value)
                .context("Gross withdrawal amount overflow")
        })
    }

    async fn select_utxos(
        &self,
        available_utxos: &[(OutPoint, TxOut)],
        total_needed: Amount,
    ) -> Result<Vec<(OutPoint, TxOut)>> {
        self.utxo_manager
            .select_utxos_by_target_value(available_utxos, total_needed)
            .await
    }

    fn validate_transaction_weight(
        &self,
        tx_fee: &TransactionWithFee,
        selected_utxos: &[(OutPoint, TxOut)],
        max_tx_weight: u64,
    ) -> Result<()> {
        let tx_weight = self.estimate_transaction_weight(
            selected_utxos.len() as u64,
            tx_fee.outputs_with_fees.len() as u64,
        );

        if tx_weight > max_tx_weight {
            anyhow::bail!(
                "Transaction with {} outputs exceeds weight limit",
                tx_fee.outputs_with_fees.len()
            );
        }
        Ok(())
    }

    fn remove_used_utxos(
        &self,
        utxos_pool: &mut Vec<(OutPoint, TxOut)>,
        selected_utxos: &[(OutPoint, TxOut)],
    ) {
        utxos_pool.retain(|(outpoint, _)| !selected_utxos.iter().any(|(used, _)| used == outpoint));
    }

    fn calculate_total_input_amount(
        &self,
        selected_utxos: &[(OutPoint, TxOut)],
        tx_index: usize,
    ) -> Result<Amount> {
        selected_utxos
            .iter()
            .try_fold(Amount::ZERO, |acc, (_, txout)| acc.checked_add(txout.value))
            .context(format!("Input amount overflow in tx index {}", tx_index))
    }

    fn calculate_total_needed(
        &self,
        tx_fee: &TransactionWithFee,
        tx_index: usize,
    ) -> Result<Amount> {
        tx_fee
            .total_value_needed
            .checked_add(tx_fee.fee)
            .context(format!("Total amount overflow in tx index {}", tx_index))
    }

    fn validate_sufficient_funds(
        &self,
        total_input_amount: Amount,
        total_needed: Amount,
        tx_index: usize,
    ) -> Result<()> {
        if total_input_amount < total_needed {
            anyhow::bail!(
                "Insufficient funds in tx index {}: have {}, need {}",
                tx_index,
                total_input_amount,
                total_needed
            );
        }
        Ok(())
    }

    fn create_inputs(&self, selected_utxos: &[(OutPoint, TxOut)]) -> Vec<TxIn> {
        selected_utxos
            .iter()
            .map(|(outpoint, _)| TxIn {
                previous_output: *outpoint,
                script_sig: ScriptBuf::default(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(),
            })
            .collect()
    }

    fn create_op_return_output(
        &self,
        tx_fee: &TransactionWithFee,
        config: &TransactionBuilderConfig,
    ) -> Result<TxOut> {
        let op_return_data = if let Some(ref data) = config.op_return_data_input_opt {
            vec![data.clone()]
        } else {
            tx_fee
                .outputs_with_fees
                .iter()
                .filter_map(|out| out.op_return_data.clone())
                .collect()
        };

        let op_return_script =
            Self::create_op_return_script(&config.op_return_prefix, op_return_data)?;

        Ok(TxOut {
            value: Amount::ZERO,
            script_pubkey: op_return_script,
        })
    }

    fn create_transaction_outputs(&self, tx_fee: &TransactionWithFee) -> Vec<TxOut> {
        tx_fee
            .outputs_with_fees
            .iter()
            .map(|out| out.output.clone())
            .collect()
    }

    fn create_change_output(
        &self,
        change_amount: Amount,
        config: &TransactionBuilderConfig,
    ) -> TxOut {
        TxOut {
            value: change_amount,
            script_pubkey: config.bridge_address.script_pubkey(),
        }
    }

    fn build_unsigned_transaction(&self, inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs,
            output: outputs,
        }
    }

    fn add_change_to_pool(
        &self,
        utxos_pool: &mut Vec<(OutPoint, TxOut)>,
        txid: bitcoin::Txid,
        outputs: &[TxOut],
        change_utxo: TxOut,
    ) {
        if change_utxo.value == Amount::ZERO {
            return;
        }
        utxos_pool.push((
            OutPoint {
                txid,
                vout: (outputs.len() - 1) as u32,
            },
            change_utxo,
        ));
    }

    fn concatenate_op_return_data(prefix: &[u8], inputs: Vec<Vec<u8>>) -> Vec<u8> {
        let total_input_size: usize = inputs.iter().map(|input| input.len()).sum();
        let mut data = Vec::with_capacity(prefix.len() + total_input_size);

        data.extend_from_slice(prefix);
        for input in inputs {
            data.extend_from_slice(&input);
        }

        data
    }

    fn encode_data(data: &[u8]) -> Result<PushBytesBuf> {
        let mut encoded_data = PushBytesBuf::with_capacity(data.len());
        encoded_data
            .extend_from_slice(data)
            .map_err(|_| anyhow::anyhow!("Failed to encode OP_RETURN data"))?;
        Ok(encoded_data)
    }

    fn validate_op_return_size(op_return: &ScriptBuf) -> Result<()> {
        let size = op_return.as_bytes().len();
        if size > 80 {
            anyhow::bail!("Invalid OP_RETURN data size {}", size);
        }
        Ok(())
    }

    // Legacy methods - kept for backward compatibility but could be removed if unused
    #[instrument(skip(self), target = "bitcoin_transaction_builder")]
    fn estimate_fee(&self, input_count: u32, output_count: u32, fee_rate: u64) -> Result<Amount> {
        let base_size = 10_u64;
        let input_size = 148_u64 * u64::from(input_count);
        let output_size = 34_u64 * u64::from(output_count);

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
                .context("Amount overflow during input count estimation")?;

            if total >= target_amount {
                break;
            }
        }

        Ok(count.saturating_add(1))
    }
}
