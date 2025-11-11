use std::{any::Any, sync::Arc};

use anyhow::Ok;
use axum::async_trait;
use bitcoin::{hashes::Hash, policy::MAX_STANDARD_TX_WEIGHT, Transaction, TxOut, Txid};
use via_btc_client::{
    indexer::{
        withdrawal::{L1Withdrawal, WithdrawalVersion},
        MessageParser,
    },
    types::{FullInscriptionMessage, TransactionWithMetadata},
};
use via_musig2::{
    fee::WithdrawalFeeStrategy,
    transaction_builder::TransactionBuilder,
    types::{TransactionBuilderConfig, TransactionOutput},
};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::{transaction::UnsignedBridgeTx, withdrawal::get_withdrawal_requests};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_types::{via_wallet::SystemWallets, L1BatchNumber};

use crate::{traits::ISession, types::SessionOperation};

const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_WI";
const WITHDRAWAL_VERSION: WithdrawalVersion = WithdrawalVersion::Version0;
const WITHDRAWAL_LIMIT: u32 = 7;

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
    async fn prepare_session(&self) -> anyhow::Result<()> {
        self.prepare_withdrawal_session().await?;
        Ok(())
    }

    async fn session(&self) -> anyhow::Result<Option<SessionOperation>> {
        let mut storage = self
            .master_connection_pool
            .connection_tagged("verifier task")
            .await?;

        // Set the minimum amount to withdraw + fee = 660 sats.
        let min_value = 660;

        let no_processed_withdrawals = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(min_value, WITHDRAWAL_LIMIT)
            .await?;

        if no_processed_withdrawals.is_empty() {
            tracing::debug!(
                "There are no withdrawal to process with a min_value {} sats",
                min_value
            );
            return Ok(None);
        }

        tracing::info!(
            "There are {} withdrawals not yet processed",
            no_processed_withdrawals.len()
        );

        let mut outputs = vec![];
        for w in no_processed_withdrawals {
            let mut op_return_data = Vec::new();
            op_return_data.extend_from_slice(&hex::decode(w.id)?);

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

        let sig_hashes = self
            .transaction_builder
            .get_tr_sighashes(&unsigned_txs[0])?;

        Ok(Some(SessionOperation::Withdrawal(
            unsigned_txs[0].clone(),
            sig_hashes,
        )))
    }

    async fn is_session_in_progress(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        let exists = self.is_bridge_session_already_processed(session_op).await?;
        Ok(!exists)
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

        Ok(!exists)
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

        let l1_withdrawals = self
            .parse_bridge_withdrawal(session_op.get_unsigned_bridge_tx().tx.clone())
            .await?;

        let withdrawals = get_withdrawal_requests(l1_withdrawals);

        transaction
            .via_withdrawal_dal()
            .mark_withdrawals_as_processed(id, &withdrawals)
            .await?;

        transaction.commit().await?;

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
                    .insert_withdrawals(&withdrawals)
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
            "Found invalid inscription type, should be FullInscriptionMessage::BridgeWithdrawal found {:?}", &msg
        ),
        };

        Ok(inscription.input.withdrawals.clone())
    }

    async fn get_system_wallets(&self) -> anyhow::Result<SystemWallets> {
        let mut storage = self.master_connection_pool.connection().await?;

        let last_processed_l1_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await?;
        let Some(system_wallets_map) = storage
            .via_wallet_dal()
            .get_system_wallets_raw(last_processed_l1_block as i64)
            .await?
        else {
            anyhow::bail!("Error load system wallets");
        };

        let system_wallets = SystemWallets::try_from(system_wallets_map)?;
        Ok(system_wallets)
    }
}
