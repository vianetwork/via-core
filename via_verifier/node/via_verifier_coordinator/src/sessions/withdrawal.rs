use std::{any::Any, collections::HashMap, sync::Arc};

use anyhow::{Context, Ok};
use axum::async_trait;
use bitcoin::{hashes::Hash, Address, Amount, TxOut, Txid};
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
        let (l1_batch_number, raw_proof_tx_id, withdrawals_to_process) =
            self.prepare_withdrawal_session().await?;

        if withdrawals_to_process.is_empty() {
            return Ok(None);
        }

        tracing::info!(
            "Found withdrawals in the l1 batch {}, total withdrawals: {}",
            l1_batch_number,
            withdrawals_to_process.len()
        );

        let proof_txid = h256_to_txid(&raw_proof_tx_id).with_context(|| "Invalid proof tx id")?;
        let unsigned_tx = self
            .create_unsigned_tx(withdrawals_to_process, proof_txid)
            .await
            .map_err(|e| {
                anyhow::format_err!("Invalid unsigned tx for batch {l1_batch_number}: {e}")
            })?;

        let sighashes = self.transaction_builder.get_tr_sighashes(&unsigned_tx)?;

        tracing::info!("New withdrawal session found for l1 batch {l1_batch_number}");

        Ok(Some(SessionOperation::Withdrawal(
            l1_batch_number,
            unsigned_tx,
            sighashes,
            raw_proof_tx_id,
        )))
    }

    async fn is_session_in_progress(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        if session_op.get_l1_batche_number() != 0 {
            let bridge_tx = self
                .master_connection_pool
                .connection_tagged("verifier withdrawal session")
                .await?
                .via_votes_dal()
                .get_vote_transaction_bridge_tx_id(session_op.get_l1_batche_number())
                .await?;

            return Ok(bridge_tx.is_none());
        }
        Ok(false)
    }

    async fn verify_message(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        if let Some((unsigned_tx, messages)) = session_op.session() {
            // Get the l1 batches finilized but withdrawals not yet processed
            if let Some((blob_id, proof_tx_id)) = self
                .master_connection_pool
                .connection_tagged("verifier withdrawal session verify message")
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

                return self
                    ._verify_sighashes(session_op.get_l1_batche_number(), unsigned_tx, messages)
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
        let bridge_txid = self
            .master_connection_pool
            .connection_tagged("verifier task")
            .await?
            .via_votes_dal()
            .get_vote_transaction_bridge_tx_id(session_op.get_l1_batche_number())
            .await?;

        Ok(bridge_txid.is_none())
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
            .mark_vote_transaction_as_processed(
                H256::from_slice(&txid.as_raw_hash().to_byte_array()),
                &session_op.get_proof_tx_id(),
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

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl WithdrawalSession {
    pub async fn prepare_withdrawal_session(
        &self,
    ) -> anyhow::Result<(i64, Vec<u8>, Vec<WithdrawalRequest>)> {
        // Get the l1 batches finilized but withdrawals not yet processed
        let l1_batches = self
            .master_connection_pool
            .connection_tagged("withdrawal session")
            .await?
            .via_votes_dal()
            .list_finalized_blocks_and_non_processed_withdrawals()
            .await?;

        if l1_batches.is_empty() {
            return Ok((0, vec![], vec![]));
        }

        let mut withdrawals_to_process: Vec<WithdrawalRequest> = Vec::new();
        let mut raw_proof_tx_id: Vec<u8> = vec![];

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
                .with_context(|| "Error to get withdrawals from DA")?;
            raw_proof_tx_id = proof_tx_id.clone();

            if !withdrawals.is_empty() {
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
                    .connection_tagged("withdrawal session")
                    .await?
                    .via_votes_dal()
                    .mark_vote_transaction_as_processed(
                        H256::zero(),
                        &raw_proof_tx_id,
                        *batch_number,
                    )
                    .await?;
                tracing::info!(
                    "There is no withdrawal to process in l1 batch {}",
                    batch_number.clone()
                );
            }
        }
        Ok((l1_batch_number, raw_proof_tx_id, withdrawals_to_process))
    }

    pub async fn create_unsigned_tx(
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
            tracing::error!("Invalid unsigned withdrawal tx output lenght");
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

    async fn _verify_sighashes(
        &self,
        l1_batch_number: i64,
        unsigned_tx: &UnsignedBridgeTx,
        sighashes_inputs: &Vec<Vec<u8>>,
    ) -> anyhow::Result<bool> {
        let sighashes = &self.transaction_builder.get_tr_sighashes(unsigned_tx)?;
        if sighashes_inputs != sighashes {
            tracing::error!(
                "Invalid transaction sighashes for session with block id {}",
                l1_batch_number
            );
            return Ok(false);
        }
        tracing::info!("Sighashes for batch {} is valid", l1_batch_number);
        Ok(true)
    }
}
