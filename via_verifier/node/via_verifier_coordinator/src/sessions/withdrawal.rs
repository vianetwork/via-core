use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Ok};
use axum::async_trait;
use bitcoin::{
    hashes::Hash,
    sighash::{Prevouts, SighashCache},
    Address, Amount, TapSighashType, TxOut, Txid,
};
use via_musig2::transaction_builder::TransactionBuilder;
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::{transaction::UnsignedBridgeTx, withdrawal::WithdrawalRequest};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_types::H256;

use crate::{traits::ISession, types::SessionOperation, utils::h256_to_txid};

const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL:";

#[derive(Debug, Clone)]
pub struct WithdrawalSession {
    master_connection_pool: ConnectionPool<Verifier>,
    transaction_builder: Arc<TransactionBuilder>,
    withdrawal_client: WithdrawalClient,
}

impl WithdrawalSession {
    pub fn new(
        master_connection_pool: ConnectionPool<Verifier>,
        transaction_builder: Arc<TransactionBuilder>,
        withdrawal_client: WithdrawalClient,
    ) -> Self {
        Self {
            master_connection_pool,
            withdrawal_client,
            transaction_builder,
        }
    }
}

#[async_trait]
impl ISession for WithdrawalSession {
    async fn session(&self) -> anyhow::Result<Option<SessionOperation>> {
        // Get the l1 batches finilized but withdrawals not yet processed
        let l1_batches = self
            .master_connection_pool
            .connection_tagged("withdrawal session")
            .await?
            .via_votes_dal()
            .get_finalized_blocks_and_non_processed_withdrawals()
            .await?;

        if l1_batches.is_empty() {
            return Ok(None);
        }

        let mut withdrawals_to_process: Vec<WithdrawalRequest> = Vec::new();
        let mut proof_txid = Txid::all_zeros();

        tracing::info!(
            "Found {} finalized unprocessed L1 batch(es) with withdrawals waiting to be processed",
            l1_batches.len()
        );

        let mut l1_batch_number: i64 = 0;
        for (batch_number, blob_id, proof_tx_id) in l1_batches.iter() {
            let withdrawals: Vec<WithdrawalRequest> = self
                .withdrawal_client
                .get_withdrawals(blob_id)
                .await
                .context("Error to get withdrawals from DA")?;

            if !withdrawals.is_empty() {
                proof_txid = h256_to_txid(proof_tx_id).context("Invalid proof tx id")?;
                l1_batch_number = *batch_number;
                withdrawals_to_process = withdrawals;
                tracing::info!(
                    "L1 batch {} includes withdrawal requests {}",
                    batch_number.clone(),
                    withdrawals_to_process.len()
                );
                break;
            } else {
                // If there is no withdrawals to process in a batch, update the status and mark it as processed
                self.master_connection_pool
                    .connection_tagged("coordinator")
                    .await?
                    .via_votes_dal()
                    .mark_vote_transaction_as_processed_withdrawals(H256::zero(), *batch_number)
                    .await
                    .context("Error to mark a vote transaction as processed")?;
                tracing::info!(
                    "There is no withdrawal to process in l1 batch {}",
                    batch_number.clone()
                );
            }
        }

        if withdrawals_to_process.is_empty() {
            return Ok(None);
        }

        tracing::info!(
            "Found withdrawals in the l1 batch {}, total withdrawals: {}",
            l1_batch_number,
            withdrawals_to_process.len()
        );

        let unsigned_tx = self
            .create_unsigned_withdrawal_tx(withdrawals_to_process, proof_txid)
            .await
            .map_err(|e| {
                anyhow::format_err!("Invalid unsigned tx for batch {l1_batch_number}: {e}")
            })?;

        let mut sighash_cache = SighashCache::new(&unsigned_tx.tx);
        let sighash_type = TapSighashType::All;
        let mut txout_list = Vec::with_capacity(unsigned_tx.utxos.len());

        for (_, txout) in unsigned_tx.utxos.clone() {
            txout_list.push(txout);
        }
        let sighash = sighash_cache
            .taproot_key_spend_signature_hash(0, &Prevouts::All(&txout_list), sighash_type)
            .context("Error taproot_key_spend_signature_hash")?;

        tracing::info!("New withdrawal session found for l1 batch {l1_batch_number}");

        Ok(Some(SessionOperation::Withdrawal(
            l1_batch_number,
            unsigned_tx,
            sighash.to_byte_array().to_vec(),
        )))
    }

    async fn is_session_in_progress(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        if session_op.get_l1_batche_number() != 0 {
            let withdrawal_tx = self
                .master_connection_pool
                .connection_tagged("coordinator api")
                .await?
                .via_votes_dal()
                .get_vote_transaction_withdrawal_tx(session_op.get_l1_batche_number())
                .await?;

            if withdrawal_tx.is_none() {
                // The withdrawal process is in progress
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn verify_message(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        if let Some((unsigned_tx, message_bytes)) = session_op.session() {
            // Get the l1 batches finilized but withdrawals not yet processed
            if let Some((blob_id, proof_tx_id)) = self
                .master_connection_pool
                .connection_tagged("verifier task")
                .await?
                .via_votes_dal()
                .get_finalized_block_and_non_processed_withdrawal(session_op.get_l1_batche_number())
                .await?
            {
                if !self
                    ._verify_withdrawals(
                        session_op.get_l1_batche_number(),
                        unsigned_tx,
                        &blob_id,
                        proof_tx_id,
                    )
                    .await?
                {
                    return Ok(false);
                }

                let message_to_sign = hex::encode(message_bytes);
                return self
                    ._verify_sighash(
                        session_op.get_l1_batche_number(),
                        unsigned_tx,
                        message_to_sign,
                    )
                    .await;
            }
        }
        Ok(false)
    }

    async fn before_process_session(&self, _: &SessionOperation) -> anyhow::Result<bool> {
        return Ok(true);
    }

    async fn before_broadcast_final_transaction(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let withdrawal_txid = self
            .master_connection_pool
            .connection_tagged("verifier task")
            .await?
            .via_votes_dal()
            .get_vote_transaction_withdrawal_tx(session_op.get_l1_batche_number())
            .await?;

        if withdrawal_txid.is_some() {
            return Ok(false);
        }

        Ok(true)
    }

    async fn after_broadcast_final_transaction(
        &self,
        txid: Txid,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        self.master_connection_pool
            .connection_tagged("verifier task")
            .await?
            .via_votes_dal()
            .mark_vote_transaction_as_processed_withdrawals(
                H256::from_slice(&txid.as_raw_hash().to_byte_array()),
                session_op.get_l1_batche_number(),
            )
            .await?;

        tracing::info!(
            "New withdrawal transaction processed, l1 batch {} musig2 tx_id {}",
            session_op.get_l1_batche_number(),
            txid
        );

        Ok(true)
    }
}

impl WithdrawalSession {
    pub async fn create_unsigned_withdrawal_tx(
        &self,
        withdrawals: Vec<WithdrawalRequest>,
        proof_txid: Txid,
    ) -> anyhow::Result<UnsignedBridgeTx> {
        // Group withdrawals by address and sum amounts
        let mut grouped_withdrawals: HashMap<Address, Amount> = HashMap::new();
        for w in withdrawals {
            *grouped_withdrawals.entry(w.address).or_insert(Amount::ZERO) = grouped_withdrawals
                .get(&w.address)
                .unwrap_or(&Amount::ZERO)
                .checked_add(w.amount)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
        }

        // Create outputs for grouped withdrawals
        let outputs: Vec<TxOut> = grouped_withdrawals
            .into_iter()
            .map(|(address, amount)| TxOut {
                value: amount,
                script_pubkey: address.script_pubkey(),
            })
            .collect();

        self.transaction_builder
            .build_transaction_with_op_return(
                outputs,
                OP_RETURN_WITHDRAW_PREFIX,
                vec![proof_txid.as_raw_hash().to_byte_array()],
            )
            .await
    }

    async fn _verify_withdrawals(
        &self,
        l1_batch_number: i64,
        unsigned_tx: &UnsignedBridgeTx,
        blob_id: &str,
        proof_tx_id: Vec<u8>,
    ) -> anyhow::Result<bool> {
        let withdrawals = self.withdrawal_client.get_withdrawals(blob_id).await?;

        // Group withdrawals by address and sum amounts
        let mut grouped_withdrawals: HashMap<String, Amount> = HashMap::new();
        for w in &withdrawals {
            let key = w.address.script_pubkey().to_string();
            *grouped_withdrawals.entry(key).or_insert(Amount::ZERO) = grouped_withdrawals
                .get(&key)
                .unwrap_or(&Amount::ZERO)
                .checked_add(w.amount)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
        }

        let len = grouped_withdrawals.len();
        if len == 0 {
            tracing::error!(
                "Invalid session, there are no withdrawals to process, l1 batch: {}",
                l1_batch_number
            );
            return Ok(false);
        }
        if len + 2 != unsigned_tx.tx.output.len() {
            // Log an error
            return Ok(false);
        }

        // Verify if all grouped_withdrawals are included with valid amount.
        for (i, txout) in unsigned_tx
            .tx
            .output
            .iter()
            .enumerate()
            .take(unsigned_tx.tx.output.len().saturating_sub(2))
        {
            let amount = &grouped_withdrawals[&txout.script_pubkey.to_string()];
            if amount != &txout.value {
                tracing::error!(
                    "Invalid request withdrawal for batch {}, index: {}",
                    l1_batch_number,
                    i
                );
                return Ok(false);
            }
        }
        tracing::info!(
            "All request withdrawals for batch {} are valid",
            l1_batch_number
        );

        // Verify the OP return
        let tx_id = h256_to_txid(&proof_tx_id)?;
        let op_return_data = TransactionBuilder::create_op_return_script(
            OP_RETURN_WITHDRAW_PREFIX,
            vec![*tx_id.as_raw_hash().as_byte_array()],
        )?;
        let op_return_tx_out = &unsigned_tx.tx.output[unsigned_tx.tx.output.len() - 2];

        if op_return_tx_out.script_pubkey.to_string() != op_return_data.to_string()
            || op_return_tx_out.value != Amount::ZERO
        {
            tracing::error!("Invalid op return data for l1 batch: {}", l1_batch_number);
            return Ok(false);
        }

        Ok(true)
    }

    async fn _verify_sighash(
        &self,
        l1_batch_number: i64,
        unsigned_tx: &UnsignedBridgeTx,
        message: String,
    ) -> anyhow::Result<bool> {
        // Verify the sighash
        let mut sighash_cache = SighashCache::new(&unsigned_tx.tx);

        let sighash_type = TapSighashType::All;
        let mut txout_list = Vec::with_capacity(unsigned_tx.utxos.len());

        for (_, txout) in unsigned_tx.utxos.clone() {
            txout_list.push(txout);
        }
        let sighash = sighash_cache
            .taproot_key_spend_signature_hash(0, &Prevouts::All(&txout_list), sighash_type)
            .context("Error taproot_key_spend_signature_hash")?;

        if message != sighash.to_string() {
            tracing::error!(
                "Invalid transaction sighash for session with block id {}",
                l1_batch_number
            );
            return Ok(false);
        }
        tracing::info!("Sighash for batch {} is valid", l1_batch_number);
        Ok(true)
    }
}
