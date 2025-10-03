use std::{any::Any, sync::Arc};

use anyhow::{Context, Ok};
use axum::async_trait;
use bitcoin::{
    hashes::Hash, policy::MAX_STANDARD_TX_WEIGHT, Address, Amount, OutPoint, Transaction, TxOut,
    Txid,
};
use indexmap::IndexMap;
use serde::Deserialize;
use via_btc_client::{
    indexer::{
        withdrawal::{L1Withdrawal, WithdrawalVersion},
        MessageParser,
    },
    traits::Serializable,
    types::{FullInscriptionMessage, TransactionWithMetadata},
};
use via_musig2::{
    fee::WithdrawalFeeStrategy,
    transaction_builder::TransactionBuilder,
    types::{TransactionBuilderConfig, TransactionOutput},
};
use via_verifier_dal::{models::withdrawal::Withdrawal, ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::{transaction::UnsignedBridgeTx, withdrawal::WithdrawalRequest};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::ViaVerifierConfig;
use zksync_types::{via_wallet::SystemWallets, L1BatchNumber, H256};

use crate::{traits::ISession, types::SessionOperation, utils::h256_to_txid};

const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_WI";
const WITHDRAWAL_VERSION: WithdrawalVersion = WithdrawalVersion::Version0;
const WITHDRAWAL_LIMIT: u32 = 7;

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
    async fn prepare_session(&self) -> anyhow::Result<()> {
        self.prepare_withdrawal_session().await?;
        Ok(())
    }

    async fn session(&self) -> anyhow::Result<Option<SessionOperation>> {
        let mut storage = self
            .master_connection_pool
            .connection_tagged("verifier task")
            .await?;

        let min_value = 0;

        let no_processed_withdrawals = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(min_value, WITHDRAWAL_LIMIT)
            .await?;

        // // Check if the transactions already processed
        // let valid_withdrawals = self
        //     .master_connection_pool
        //     .connection_tagged("verifier task")
        //     .await?
        //     .via_withdrawal_dal()
        //     .check_if_withdrawals_are_processed(&withdrawals_to_process)
        //     .await?;

        // let filtered_withdrawals = withdrawals_to_process
        //     .into_iter()
        //     .zip(valid_withdrawals.into_iter())
        //     .filter(|(_, is_valid)| *is_valid)
        //     .map(|(s, _)| s)
        //     .collect::<Vec<WithdrawalRequest>>();

        if no_processed_withdrawals.is_empty() {
            tracing::info!(
                "There are no withdrawal to process with a min_value {} sats",
                min_value
            );
            return Ok(None);
        }

        tracing::info!(
            "There are {} withdrawals not yet processed",
            no_processed_withdrawals.len()
        );

        // let proof_txid = h256_to_txid(&raw_proof_tx_id).with_context(|| "Invalid proof tx id")?;

        // let votable_tx_id_opt = self.get_votable_tx_id(&raw_proof_tx_id).await?;
        // let Some(votable_tx_id) = votable_tx_id_opt else {
        //     return Ok(None);
        // };

        let mut outputs = vec![];
        for w in no_processed_withdrawals {
            let mut op_return_data = Vec::new();
            op_return_data.extend_from_slice(&hex::decode(w.id)?);
            op_return_data.extend_from_slice(&w.l2_tx_log_index.to_be_bytes());

            outputs.push(TransactionOutput {
                output: TxOut {
                    value: w.amount,
                    script_pubkey: w.receiver.script_pubkey(),
                },
                op_return_data: Some(op_return_data),
            });
        }

        let mut op_return_prefix = Vec::new();
        op_return_prefix.extend_from_slice(OP_RETURN_WITHDRAW_PREFIX);
        op_return_prefix.push(WITHDRAWAL_VERSION as u8);

        let config = TransactionBuilderConfig {
            fee_strategy: Arc::new(WithdrawalFeeStrategy::new()),
            max_tx_weight: MAX_STANDARD_TX_WEIGHT as u64,
            max_output_per_tx: WITHDRAWAL_LIMIT as usize,
            op_return_prefix,
            bridge_address: self.get_system_wallets().await?.bridge,
            default_fee_rate_opt: None,
            default_available_utxos_opt: None,
            op_return_data_input_opt: None,
        };

        let unsigned_txs = self
            .transaction_builder
            .build_transaction_with_op_return(outputs, config)
            .await?;

        if unsigned_txs.is_empty() {
            return Ok(None);
        }

        // let (index, unsigned_txs) =
        //     // if let Some((index, unsigned_txs)) = self.get_unsigned_txs(&raw_proof_tx_id).await? {
        //     //     (index, unsigned_txs)
        //     // } else {
        //     let (index, unsigned_txs) = self
        //         .create_unsigned_txs(
        //             filtered_withdrawals,
        //             proof_txid,
        //             None,
        //             None,
        //             self.get_system_wallets().await?,
        //         )
        //         .await
        //         .map_err(|e| {
        //             anyhow::format_err!("Invalid unsigned tx for batch {l1_batch_number}: {e}")
        //         })?;

        //     self.insert_bridge_tx(&raw_proof_tx_id, unsigned_txs.clone())
        //         .await?;
        //     (index, unsigned_txs)
        //     // };

        let sig_hashes = self
            .transaction_builder
            .get_tr_sighashes(&unsigned_txs[0])?;

        Ok(Some(SessionOperation::Withdrawal(
            unsigned_txs[0].clone(),
            sig_hashes,
        )))
    }

    async fn is_session_in_progress(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        // if session_op.get_l1_batch_number() != 0 {
        //     let bridge_txs = self
        //         .master_connection_pool
        //         .connection_tagged("verifier withdrawal session")
        //         .await?
        //         .via_votes_dal()
        //         .get_vote_transaction_bridge_tx(
        //             session_op.get_l1_batch_number(),
        //             session_op.index(),
        //         )
        //         .await?;

        //     return Ok(bridge_txs.is_empty());
        // }
        Ok(false)
    }

    async fn verify_message(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        let messages = session_op.get_message_to_sign();
        let unsigned_tx = session_op.get_unsigned_bridge_tx();

        if !self._verify_withdrawals(&session_op).await? {
            tracing::error!("Failed to verify session withdrawals");
            return Ok(false);
        }

        if !self._verify_sighashes(&unsigned_tx, &messages).await? {
            tracing::error!("Failed to verify session message");
            return Ok(false);
        }

        Ok(true)
    }

    async fn before_process_session(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        let exists = self.is_bridge_session_already_processed(session_op).await?;
        return Ok(!exists);
    }

    async fn before_broadcast_final_transaction(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let tx_id = session_op
            .get_unsigned_bridge_tx()
            .txid
            .as_byte_array()
            .to_vec();

        let exists = self
            .master_connection_pool
            .connection_tagged("verifier task")
            .await?
            .via_withdrawal_dal()
            .bridge_withdrawal_exists(&tx_id)
            .await?;

        Ok(exists)
    }

    async fn after_broadcast_final_transaction(
        &self,
        txid: Txid,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let mut storage = self
            .master_connection_pool
            .connection_tagged("verifier task")
            .await?;
        let mut transaction = storage.start_transaction().await?;

        let id = transaction
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&txid.as_byte_array().to_vec())
            .await?;

        // let parser = MessageParser::new(self.withdrawal_client.network);
        // let system_wallets = self.get_system_wallets().await?;
        // let Some(msg) = parser
        //     .parse_bridge_transaction(
        //         &session_op.get_unsigned_bridge_tx().tx.clone(),
        //         0,
        //         &system_wallets,
        //     )
        //     .first()
        // else {
        //     anyhow::bail!("Could not parse the transaction");
        // };

        // let inscription = match msg {
        //     FullInscriptionMessage::BridgeWithdrawal(inscription) => inscription,
        //     _ => anyhow::bail!("Found invalid inscription type, should be FullInscriptionMessage::BridgeWithdrawal")
        // };

        let l1_withdrawals = self
            .parse_bridge_withdrawal(session_op.get_unsigned_bridge_tx().tx.clone())
            .await?;

        for w in l1_withdrawals {
            transaction
                .via_withdrawal_dal()
                .mark_withdrawal_as_processed(id, &w.into())
                .await?;
        }

        transaction.commit().await?;

        // let votable_tx_id = self
        //     .get_votable_tx_id(&session_op.get_proof_tx_id())
        //     .await?
        //     .ok_or_else(|| anyhow::anyhow!("Votable transaction does not exist"))?;

        // let hash_bytes = txid.to_byte_array().to_vec();

        // self.master_connection_pool
        //     .connection_tagged("verifier task")
        //     .await?
        //     .via_bridge_dal()
        //     .update_bridge_tx(
        //         &session_op.get_proof_tx_id(),
        //         session_op.index() as i64,
        //         &hash_bytes,
        //     )
        //     .await?;

        self.transaction_builder
            .utxo_manager_insert_transaction(session_op.get_unsigned_bridge_tx().tx.clone())
            .await;

        tracing::info!("Final withdrawal transaction broadcasted: txid {}", txid);

        Ok(true)
    }

    async fn is_bridge_session_already_processed(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let tx_id = session_op
            .get_unsigned_bridge_tx()
            .txid
            .as_byte_array()
            .to_vec();

        let exists = self
            .master_connection_pool
            .connection_tagged("withdrawal session")
            .await?
            .via_withdrawal_dal()
            .bridge_withdrawal_exists(&tx_id)
            .await?;

        Ok(exists)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl WithdrawalSession {
    pub async fn prepare_withdrawal_session(&self) -> anyhow::Result<()> {
        let mut storage = self
            .master_connection_pool
            .connection_tagged("verifier task")
            .await?;

        // Get the l1 batches finalized but withdrawals not yet processed
        let l1_batches = storage
            .via_withdrawal_dal()
            .list_finalized_blocks_with_no_bridge_withdrawal()
            .await?;

        if l1_batches.is_empty() {
            return Ok(());
        }

        tracing::info!(
            "Found {} finalized unprocessed L1 batch(es) with withdrawals waiting to be processed",
            l1_batches.len()
        );

        let mut transaction = storage.start_transaction().await?;

        for (batch_number, blob_id, proof_tx_id) in l1_batches.iter() {
            let withdrawals = self
                .withdrawal_client
                .get_withdrawals(blob_id, L1BatchNumber(batch_number.clone() as u32))
                .await?;

            transaction
                .via_withdrawal_dal()
                .insert_l1_batch_bridge_withdrawals(&proof_tx_id.clone())
                .await?;

            if !withdrawals.is_empty() {
                transaction
                    .via_withdrawal_dal()
                    .insert_withdrawals(withdrawals.clone())
                    .await?;
            }

            tracing::info!(
                "L1 batch number {} contains {} withdrawal requests",
                batch_number.clone(),
                withdrawals.len()
            );
        }

        transaction.commit().await?;

        Ok(())
    }

    // pub async fn get_unsigned_txs(
    //     &self,
    //     raw_proof_tx_id: &[u8],
    // ) -> anyhow::Result<Option<(usize, Vec<UnsignedBridgeTx>)>> {
    //     let db_unsigned_bridge_txs = self
    //         .master_connection_pool
    //         .connection_tagged("verifier")
    //         .await?
    //         .via_bridge_dal()
    //         .get_vote_transaction_bridge_txs(raw_proof_tx_id)
    //         .await?;

    //     if let Some((index, _)) = db_unsigned_bridge_txs
    //         .iter()
    //         .enumerate()
    //         .find(|(_, (_, hash))| hash.is_empty())
    //     {
    //         let unsigned_bridge_txs_bytes: Vec<Vec<u8>> = db_unsigned_bridge_txs
    //             .iter()
    //             .map(|(data, _)| data.clone())
    //             .collect();

    //         return Ok(Some((
    //             index,
    //             UnsignedBridgeTx::to_vec(unsigned_bridge_txs_bytes),
    //         )));
    //     }

    //     Ok(None)
    // }

    // pub async fn create_unsigned_txs(
    //     &self,
    //     withdrawals: Vec<WithdrawalRequest>,
    //     proof_txid: Txid,
    //     default_fee_rate: Option<u64>,
    //     default_available_utxos: Option<Vec<(OutPoint, TxOut)>>,
    //     bridge_address: Address,
    // ) -> anyhow::Result<(usize, Vec<UnsignedBridgeTx>)> {
    //     // Group withdrawals by address and sum amounts
    //     // let grouped_withdrawals = WithdrawalRequest::group_withdrawals_by_address(withdrawals)?;

    //     // // Create outputs for grouped withdrawals
    //     // let outputs: Vec<TxOut> = grouped_withdrawals
    //     //     .into_iter()
    //     //     .map(|(address, amount)| TxOut {
    //     //         value: amount,
    //     //         script_pubkey: address.script_pubkey(),
    //     //     })
    //     //     .collect();

    //     let outputs = withdrawals
    //         .iter()
    //         .map(|w| TxOut {
    //             script_pubkey: w.receiver.script_pubkey(),
    //             value: w.amount,
    //         })
    //         .collect::<Vec<TxOut>>();

    //     let decoded: Vec<Vec<u8>> = withdrawals
    //         .iter()
    //         .map(|w| hex::decode(&w.l2_tx_hash).unwrap())
    //         .collect();

    //     let op_return_data: Vec<&[u8]> = decoded.iter().map(|bytes| &bytes[..8]).collect();

    //     let unsigned_bridge_txs = vec![];
    //     // let unsigned_bridge_txs = self
    //     //     .transaction_builder
    //     //     .build_transaction_with_op_return(
    //     //         outputs,
    //     //         OP_RETURN_WITHDRAW_PREFIX,
    //     //         // vec![&proof_txid.as_raw_hash().to_byte_array().to_vec()],
    //     //         op_return_data,
    //     //         Arc::new(WithdrawalFeeStrategy::new()),
    //     //         default_fee_rate,
    //     //         default_available_utxos,
    //     //         self.verifier_config.max_tx_weight(),
    //     //         bridge_address,
    //     //     )
    //     //     .await?;

    //     Ok((0, unsigned_bridge_txs))
    // }

    async fn _verify_withdrawals(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        // Verify the fee used to build the withdrawal transaction.
        let fee_rate = self
            .transaction_builder
            .utxo_manager
            .get_btc_client()
            .get_fee_rate(1)
            .await?;

        let used_fee_rate = session_op.get_unsigned_bridge_tx().fee_rate;

        // Acceptable if difference is within ±1 sat/vbyte
        if (used_fee_rate as i32 - fee_rate as i32).abs() > 1 {
            tracing::error!("Fee mismatch: used={}, network={}", used_fee_rate, fee_rate);
            return Ok(false);
        }

        let l1_withdrawals = self
            .parse_bridge_withdrawal(session_op.get_unsigned_bridge_tx().tx.clone())
            .await?;

        let mut storage = self
            .master_connection_pool
            .connection_tagged("verifier task")
            .await?;

        for w in l1_withdrawals {
            let processed = storage
                .via_withdrawal_dal()
                .check_if_withdrawal_exists_unprocessed(&w.clone().into())
                .await?;

            if processed {
                tracing::error!(
                    "Withdrawal already processed or not exists was found in this session, tx hash id: {:?}",
                    w.l2_meta.l2_id
                );
                return Ok(false);
            }
        }

        // let selected_utxos = session_operation
        //     .unsigned_txs()
        //     .iter()
        //     .flat_map(|tx| tx.utxos.clone())
        //     .collect::<Vec<_>>();

        // let (_, recovered_unsigned_txs) = self
        //     .create_unsigned_txs(
        //         withdrawals.clone(),
        //         proof_txid,
        //         Some(used_fee_rate),
        //         Some(selected_utxos),
        //         self.get_system_wallets().await?,
        //     )
        //     .await?;

        // if recovered_unsigned_txs != *session_operation.unsigned_txs() {
        //     tracing::error!("Mismatch in unsigned withdrawal transactions");
        //     return Ok(false);
        // }

        // let Some(votable_tx_id) = self.get_votable_tx_id(&raw_proof_tx_id).await? else {
        //     tracing::error!(
        //         "Theres is no votable transaction with proof_tx_id {}",
        //         raw_proof_tx_id.to_hex_string(Case::Lower)
        //     );
        //     return Ok(false);
        // };

        // if self.get_unsigned_txs(&raw_proof_tx_id).await?.is_none() {
        //     self.insert_bridge_tx(&raw_proof_tx_id, recovered_unsigned_txs)
        //         .await?;
        // }

        // // Group withdrawals by address
        // let grouped_withdrawals: indexmap::IndexMap<bitcoin::Address, Amount> =
        //     WithdrawalRequest::group_withdrawals_by_address(withdrawals)?;

        // for tx in session_operation.unsigned_txs() {
        //     let fee_per_user = tx.get_fee_per_user();

        //     let adjusted_withdrawals: IndexMap<String, Amount> = grouped_withdrawals
        //         .iter()
        //         .filter_map(|(addr, amount)| {
        //             if *amount > fee_per_user {
        //                 Some((addr.script_pubkey().to_string(), *amount))
        //             } else {
        //                 None
        //             }
        //         })
        //         .collect();

        //     for (i, txout) in tx
        //         .tx
        //         .output
        //         .iter()
        //         .take(tx.tx.output.len().saturating_sub(2))
        //         .enumerate()
        //     {
        //         let addr_str = txout.script_pubkey.to_string();
        //         let Some(total_amount) = adjusted_withdrawals.get(&addr_str) else {
        //             tracing::error!("Missing withdrawal output for address {}", addr_str);
        //             return Ok(false);
        //         };

        //         let expected_value = *total_amount - fee_per_user;
        //         if txout.value != expected_value {
        //             tracing::error!(
        //                 "Incorrect withdrawal value in batch {}, index {}: expected {}, got {}",
        //                 session_operation.get_l1_batch_number(),
        //                 i,
        //                 expected_value,
        //                 txout.value
        //             );
        //             return Ok(false);
        //         }
        //     }
        // }

        // tracing::info!(
        //     "Withdrawals verified for L1 batch {}",
        //     session_operation.get_l1_batch_number()
        // );

        Ok(true)
    }

    async fn _verify_sighashes(
        &self,
        unsigned_tx: &UnsignedBridgeTx,
        sighashes_inputs: &Vec<Vec<u8>>,
    ) -> anyhow::Result<bool> {
        let sig_hashes = &self.transaction_builder.get_tr_sighashes(unsigned_tx)?;
        if sighashes_inputs != sig_hashes {
            tracing::error!("Invalid transaction sig_hashes for session",);
            return Ok(false);
        }
        tracing::info!("All sig_hashes are valid");
        Ok(true)
    }

    // async fn insert_bridge_tx(
    //     &self,
    //     raw_proof_tx_id: &[u8],
    //     unsigned_bridge_txs: Vec<UnsignedBridgeTx>,
    // ) -> anyhow::Result<()> {
    //     let mut data = vec![];
    //     for unsigned_bridge_tx in unsigned_bridge_txs.iter() {
    //         data.push(unsigned_bridge_tx.to_bytes());
    //     }

    //     self.master_connection_pool
    //         .connection_tagged("verifier")
    //         .await?
    //         .via_bridge_dal()
    //         .insert_bridge_txs(raw_proof_tx_id, &data)
    //         .await?;

    //     Ok(())
    // }

    // async fn get_votable_tx_id(&self, proof_txid: &[u8]) -> anyhow::Result<Option<i64>> {
    //     let votable_tx_id = self
    //         .master_connection_pool
    //         .connection_tagged("verifier")
    //         .await?
    //         .via_votes_dal()
    //         .get_votable_transaction_id(proof_txid)
    //         .await?;
    //     Ok(votable_tx_id)
    // }

    async fn parse_bridge_withdrawal(&self, tx: Transaction) -> anyhow::Result<Vec<L1Withdrawal>> {
        let mut parser = MessageParser::new(self.withdrawal_client.network.clone());
        let system_wallets = self.get_system_wallets().await?;

        let messages = parser.parse_bridge_transaction(
            &mut TransactionWithMetadata {
                tx,
                output_vout: None,
                tx_index: 0,
            },
            0,
            &system_wallets,
        );

        let Some(msg) = messages.first() else {
            anyhow::bail!("Could not parse the transaction");
        };

        let inscription = match msg {
            FullInscriptionMessage::BridgeWithdrawal(inscription) => inscription,
            _ => anyhow::bail!(
            "Found invalid inscription type, should be FullInscriptionMessage::BridgeWithdrawal"
        ),
        };

        Ok(inscription.input.withdrawals.clone())
    }

    async fn get_system_wallets(&self) -> anyhow::Result<SystemWallets> {
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
        Ok(system_wallets)
    }
}
