use std::{any::Any, sync::Arc};

use anyhow::{Context, Ok};
use axum::async_trait;
use bitcoin::{
    hashes::Hash,
    hex::{Case, DisplayHex},
    Address, Amount, OutPoint, TxOut, Txid,
};
use indexmap::IndexMap;
use via_btc_client::traits::Serializable;
use via_musig2::{fee::WithdrawalFeeStrategy, transaction_builder::TransactionBuilder};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::{transaction::UnsignedBridgeTx, withdrawal::WithdrawalRequest};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::ViaVerifierConfig;
use zksync_types::{via_wallet::SystemWallets, H256};

use crate::{traits::ISession, types::SessionOperation, utils::h256_to_txid};

const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL:";

#[derive(Debug, Clone)]
pub struct WithdrawalSession {
    verifier_config: ViaVerifierConfig,
    master_connection_pool: ConnectionPool<Verifier>,
    transaction_builder: Arc<TransactionBuilder>,
    withdrawal_client: WithdrawalClient,
}

impl WithdrawalSession {
    pub fn new(
        verifier_config: ViaVerifierConfig,
        master_connection_pool: ConnectionPool<Verifier>,
        transaction_builder: Arc<TransactionBuilder>,
        withdrawal_client: WithdrawalClient,
    ) -> Self {
        Self {
            verifier_config,
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

        let votable_tx_id_opt = self.get_votable_tx_id(&raw_proof_tx_id).await?;
        let Some(votable_tx_id) = votable_tx_id_opt else {
            return Ok(None);
        };

        let (index, unsigned_txs) =
            if let Some((index, unsigned_txs)) = self.get_unsigned_txs(votable_tx_id).await? {
                (index, unsigned_txs)
            } else {
                let (index, unsigned_txs) = self
                    .create_unsigned_txs(
                        withdrawals_to_process,
                        proof_txid,
                        None,
                        None,
                        self.get_bridget_address().await?,
                    )
                    .await
                    .map_err(|e| {
                        anyhow::format_err!("Invalid unsigned tx for batch {l1_batch_number}: {e}")
                    })?;

                self.insert_bridge_tx(votable_tx_id, unsigned_txs.clone())
                    .await?;
                (index, unsigned_txs)
            };

        let sighashes = self
            .transaction_builder
            .get_tr_sighashes(&unsigned_txs[index])?;

        tracing::info!("New withdrawal session found for l1 batch {l1_batch_number}");

        Ok(Some(SessionOperation::Withdrawal(
            l1_batch_number,
            unsigned_txs,
            sighashes,
            raw_proof_tx_id,
            index,
        )))
    }

    async fn is_session_in_progress(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        if session_op.get_l1_batch_number() != 0 {
            let bridge_txs = self
                .master_connection_pool
                .connection_tagged("verifier withdrawal session")
                .await?
                .via_votes_dal()
                .get_vote_transaction_bridge_tx(
                    session_op.get_l1_batch_number(),
                    session_op.index(),
                )
                .await?;

            return Ok(bridge_txs.is_empty());
        }
        Ok(false)
    }

    async fn verify_message(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        if let Some((unsigned_tx, messages)) = session_op.session() {
            // Get the l1 batches finalized but withdrawals not yet processed
            if let Some((blob_id, proof_tx_id)) = self
                .master_connection_pool
                .connection_tagged("verifier withdrawal session verify message")
                .await?
                .via_votes_dal()
                .get_finalized_block_and_non_processed_withdrawal(session_op.get_l1_batch_number())
                .await?
            {
                if !self
                    ._verify_withdrawals(&session_op, &blob_id, proof_tx_id)
                    .await?
                {
                    tracing::error!("Failed to verify withdrawals");
                    return Ok(false);
                }

                return self
                    ._verify_sighashes(session_op.get_l1_batch_number(), &unsigned_tx, messages)
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
            .get_vote_transaction_bridge_tx(session_op.get_l1_batch_number(), session_op.index())
            .await?;

        Ok(bridge_txid.is_empty())
    }

    async fn after_broadcast_final_transaction(
        &self,
        txid: Txid,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let votable_tx_id = self
            .get_votable_tx_id(&session_op.get_proof_tx_id())
            .await?
            .ok_or_else(|| anyhow::anyhow!("Votable transaction does not exist"))?;

        let hash_bytes = txid.to_byte_array().to_vec();

        self.master_connection_pool
            .connection_tagged("verifier task")
            .await?
            .via_bridge_dal()
            .update_bridge_tx(votable_tx_id, session_op.index() as i64, &hash_bytes)
            .await?;

        self.transaction_builder
            .utxo_manager_insert_transaction(session_op.get_unsigned_bridge_tx().tx.clone())
            .await;

        tracing::info!(
            "Final withdrawal transaction broadcasted: L1 batch {}, txid {}",
            session_op.get_l1_batch_number(),
            txid
        );

        Ok(true)
    }

    async fn is_bridge_session_already_processed(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let bridge_tx_id = self
            .master_connection_pool
            .connection_tagged("verifier")
            .await?
            .via_votes_dal()
            .get_vote_transaction_bridge_tx(session_op.get_l1_batch_number(), session_op.index())
            .await?;

        Ok(!bridge_tx_id.is_empty())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl WithdrawalSession {
    pub async fn prepare_withdrawal_session(
        &self,
    ) -> anyhow::Result<(i64, Vec<u8>, Vec<WithdrawalRequest>)> {
        // Get the l1 batches finalized but withdrawals not yet processed
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
                let votable_tx_id = self
                    .get_votable_tx_id(&raw_proof_tx_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Votable transaction does not exist"))?;

                self.master_connection_pool
                    .connection_tagged("verifier task")
                    .await?
                    .via_bridge_dal()
                    .insert_bridge_tx(votable_tx_id, Some(H256::zero().as_bytes()), None)
                    .await?;

                tracing::info!(
                    "There is no withdrawal to process in l1 batch {} votable_tx_id {}",
                    batch_number.clone(),
                    votable_tx_id
                );
            }
        }
        Ok((l1_batch_number, raw_proof_tx_id, withdrawals_to_process))
    }

    pub async fn get_unsigned_txs(
        &self,
        votable_tx_id: i64,
    ) -> anyhow::Result<Option<(usize, Vec<UnsignedBridgeTx>)>> {
        let db_unsigned_bridge_txs = self
            .master_connection_pool
            .connection_tagged("verifier")
            .await?
            .via_bridge_dal()
            .get_vote_transaction_bridge_txs(votable_tx_id)
            .await?;

        if let Some((index, _)) = db_unsigned_bridge_txs
            .iter()
            .enumerate()
            .find(|(_, (_, hash))| hash.is_empty())
        {
            let unsigned_bridge_txs_bytes: Vec<Vec<u8>> = db_unsigned_bridge_txs
                .iter()
                .map(|(data, _)| data.clone())
                .collect();

            return Ok(Some((
                index,
                UnsignedBridgeTx::to_vec(unsigned_bridge_txs_bytes),
            )));
        }

        Ok(None)
    }

    pub async fn create_unsigned_txs(
        &self,
        withdrawals: Vec<WithdrawalRequest>,
        proof_txid: Txid,
        default_fee_rate: Option<u64>,
        default_available_utxos: Option<Vec<(OutPoint, TxOut)>>,
        bridge_address: Address,
    ) -> anyhow::Result<(usize, Vec<UnsignedBridgeTx>)> {
        // Group withdrawals by address and sum amounts
        let grouped_withdrawals = WithdrawalRequest::group_withdrawals_by_address(withdrawals)?;

        // Create outputs for grouped withdrawals
        let outputs: Vec<TxOut> = grouped_withdrawals
            .into_iter()
            .map(|(address, amount)| TxOut {
                value: amount,
                script_pubkey: address.script_pubkey(),
            })
            .collect();

        let unsigned_bridge_txs = self
            .transaction_builder
            .build_transaction_with_op_return(
                outputs,
                OP_RETURN_WITHDRAW_PREFIX,
                vec![&proof_txid.as_raw_hash().to_byte_array().to_vec()],
                Arc::new(WithdrawalFeeStrategy::new()),
                default_fee_rate,
                default_available_utxos,
                self.verifier_config.max_tx_weight(),
                bridge_address,
            )
            .await?;

        Ok((0, unsigned_bridge_txs))
    }

    async fn _verify_withdrawals(
        &self,
        session_operation: &SessionOperation,
        blob_id: &str,
        raw_proof_tx_id: Vec<u8>,
    ) -> anyhow::Result<bool> {
        let withdrawals = self.withdrawal_client.get_withdrawals(blob_id).await?;

        // Verify the fee used to build the withdrawal transaction.
        let fee_rate = self
            .transaction_builder
            .utxo_manager
            .get_btc_client()
            .get_fee_rate(1)
            .await?;

        let used_fee_rate = session_operation.get_unsigned_bridge_tx().fee_rate;

        // Acceptable if difference is within ±1 sat/vbyte
        if (used_fee_rate as i32 - fee_rate as i32).abs() > 1 {
            tracing::error!("Fee mismatch: used={}, network={}", used_fee_rate, fee_rate);
            return Ok(false);
        }

        let proof_txid = h256_to_txid(&raw_proof_tx_id).with_context(|| "Invalid proof tx id")?;

        let selected_utxos = session_operation
            .unsigned_txs()
            .iter()
            .flat_map(|tx| tx.utxos.clone())
            .collect::<Vec<_>>();

        let (_, recovered_unsigned_txs) = self
            .create_unsigned_txs(
                withdrawals.clone(),
                proof_txid,
                Some(used_fee_rate),
                Some(selected_utxos),
                self.get_bridget_address().await?,
            )
            .await?;

        if recovered_unsigned_txs != *session_operation.unsigned_txs() {
            tracing::error!("Mismatch in unsigned withdrawal transactions");
            return Ok(false);
        }

        let Some(votable_tx_id) = self.get_votable_tx_id(&raw_proof_tx_id).await? else {
            tracing::error!(
                "Theres is no votable transaction with proof_tx_id {}",
                raw_proof_tx_id.to_hex_string(Case::Lower)
            );
            return Ok(false);
        };

        if self.get_unsigned_txs(votable_tx_id).await?.is_none() {
            self.insert_bridge_tx(votable_tx_id, recovered_unsigned_txs)
                .await?;
        }

        // Group withdrawals by address
        let grouped_withdrawals: indexmap::IndexMap<bitcoin::Address, Amount> =
            WithdrawalRequest::group_withdrawals_by_address(withdrawals)?;

        for tx in session_operation.unsigned_txs() {
            let fee_per_user = tx.get_fee_per_user();

            let adjusted_withdrawals: IndexMap<String, Amount> = grouped_withdrawals
                .iter()
                .filter_map(|(addr, amount)| {
                    if *amount > fee_per_user {
                        Some((addr.script_pubkey().to_string(), *amount))
                    } else {
                        None
                    }
                })
                .collect();

            for (i, txout) in tx
                .tx
                .output
                .iter()
                .take(tx.tx.output.len().saturating_sub(2))
                .enumerate()
            {
                let addr_str = txout.script_pubkey.to_string();
                let Some(total_amount) = adjusted_withdrawals.get(&addr_str) else {
                    tracing::error!("Missing withdrawal output for address {}", addr_str);
                    return Ok(false);
                };

                let expected_value = *total_amount - fee_per_user;
                if txout.value != expected_value {
                    tracing::error!(
                        "Incorrect withdrawal value in batch {}, index {}: expected {}, got {}",
                        session_operation.get_l1_batch_number(),
                        i,
                        expected_value,
                        txout.value
                    );
                    return Ok(false);
                }
            }
        }

        tracing::info!(
            "Withdrawals verified for L1 batch {}",
            session_operation.get_l1_batch_number()
        );

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

    async fn insert_bridge_tx(
        &self,
        votable_tx_id: i64,
        unsigned_bridge_txs: Vec<UnsignedBridgeTx>,
    ) -> anyhow::Result<()> {
        let mut data = vec![];
        for unsigned_bridge_tx in unsigned_bridge_txs.iter() {
            data.push(unsigned_bridge_tx.to_bytes());
        }

        self.master_connection_pool
            .connection_tagged("verifier")
            .await?
            .via_bridge_dal()
            .insert_bridge_txs(votable_tx_id, &data)
            .await?;

        Ok(())
    }

    async fn get_votable_tx_id(&self, proof_txid: &[u8]) -> anyhow::Result<Option<i64>> {
        let votable_tx_id = self
            .master_connection_pool
            .connection_tagged("verifier")
            .await?
            .via_votes_dal()
            .get_votable_transaction_id(proof_txid)
            .await?;
        Ok(votable_tx_id)
    }

    async fn get_bridget_address(&self) -> anyhow::Result<Address> {
        let Some(system_wallets_map) = self
            .master_connection_pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await?
        else {
            anyhow::bail!("Error load system wallets");
        };

        let system_wallets = SystemWallets::try_from(system_wallets_map)?;
        Ok(system_wallets.bridge)
    }
}
