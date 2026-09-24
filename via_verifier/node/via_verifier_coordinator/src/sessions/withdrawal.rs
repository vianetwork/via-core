use std::{any::Any, collections::HashSet, sync::Arc};

use anyhow::Context;
use axum::async_trait;
use bitcoin::{hashes::Hash, Transaction, TxOut};
use via_btc_client::{
    indexer::{withdrawal::L1Withdrawal, MessageParser},
    types::{FullInscriptionMessage, TransactionWithMetadata},
};
use via_musig2::{
    transaction_builder::TransactionBuilder,
    types::{TransactionBuilderConfig, TransactionOutput},
};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::{
    withdrawal::WithdrawalRequest,
    withdrawal_observation::{ObservedWithdrawal, WithdrawalObservation},
};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_types::{via_wallet::SystemWallets, L1BatchNumber};

use crate::{traits::ISession, types::SessionOperation};

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
            transaction_builder,
            withdrawal_client,
        }
    }

    fn requested_outputs(requests: &[WithdrawalRequest]) -> anyhow::Result<Vec<TransactionOutput>> {
        requests
            .iter()
            .map(|request| {
                Ok(TransactionOutput {
                    output: TxOut {
                        value: request.amount,
                        script_pubkey: request.receiver.script_pubkey(),
                    },
                    op_return_data: Some(
                        hex::decode(&request.id)
                            .context("Invalid retained withdrawal reference")?,
                    ),
                })
            })
            .collect()
    }

    pub async fn wallet_script(&self) -> anyhow::Result<Vec<u8>> {
        Ok(self
            .get_system_wallets()
            .await?
            .bridge
            .script_pubkey()
            .into_bytes())
    }

    pub async fn chain_id(&self) -> anyhow::Result<u64> {
        let wallet = self.wallet_script().await?;
        self.master_connection_pool
            .connection_tagged("withdrawal authority domain")
            .await?
            .via_withdrawal_dal()
            .withdrawal_authority_chain_id(&wallet)
            .await
    }

    /// Authorize fixed transaction content, then let durable admission reserve its exact facts.
    /// Like sBTC's approved-sighash handoff, this does not authorize another proposal:
    /// https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/src/transaction_signer.rs#L1218-L1245
    pub async fn authorize_withdrawal(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<Vec<WithdrawalRequest>> {
        let candidate = session_op.get_unsigned_bridge_tx();
        let config = TransactionBuilderConfig::withdrawal(self.get_system_wallets().await?.bridge);
        let wallet = config.bridge_address.script_pubkey();
        let payments = self.parse_bridge_withdrawal(candidate.tx.clone()).await?;
        anyhow::ensure!(!payments.is_empty(), "Withdrawal proposal has no payments");
        let references: Vec<_> = payments
            .iter()
            .map(|payment| payment.l2_meta.l2_id.clone())
            .collect();
        let requests = self
            .master_connection_pool
            .connection_tagged("withdrawal authorization")
            .await?
            .via_withdrawal_dal()
            .load_expected_withdrawals(wallet.as_bytes(), &references)
            .await?;
        anyhow::ensure!(
            requests.iter().all(|request| request
                .receiver
                .as_unchecked()
                .is_valid_for_network(self.withdrawal_client.network)),
            "Retained withdrawal recipient belongs to a different Bitcoin network"
        );

        let client = self.transaction_builder.utxo_manager.get_btc_client();
        let network_fee = client
            .get_fee_rate(1)
            .await
            .context("Withdrawal fee estimate unavailable")?;
        anyhow::ensure!(
            candidate.fee_rate.abs_diff(network_fee) <= 1,
            "Withdrawal fee rate outside accepted tolerance"
        );
        let mut parents = session_op.get_parent_transactions().to_vec();
        let mut seen: HashSet<_> = parents.iter().map(Transaction::compute_txid).collect();
        for (outpoint, _) in &candidate.utxos {
            if seen.insert(outpoint.txid) {
                parents.push(
                    client
                        .get_transaction(&outpoint.txid)
                        .await
                        .with_context(|| {
                            format!("Required withdrawal parent {} unavailable", outpoint.txid)
                        })?,
                );
            }
        }
        self.transaction_builder
            .verify_fixed_bridge_tx(
                &candidate,
                Self::requested_outputs(&requests)?,
                &config,
                &parents,
            )
            .context("Withdrawal transaction differs from retained authority")?;
        anyhow::ensure!(
            self.transaction_builder.get_tr_sighashes(&candidate)?
                == session_op.get_message_to_sign(),
            "Withdrawal signing messages differ from the authorized transaction"
        );
        Ok(requests)
    }

    pub async fn prepare_withdrawal_session(&self) -> anyhow::Result<()> {
        let wallet = self.wallet_script().await?;
        let batches = self
            .master_connection_pool
            .connection_tagged("withdrawal import")
            .await?
            .via_withdrawal_dal()
            .list_finalized_blocks_with_no_bridge_withdrawal(&wallet)
            .await?;
        for (number, blob_id, proof_tx_id) in batches {
            let imported: anyhow::Result<()> = async {
                // Source I/O precedes the short atomic import; a missing batch never commits a marker.
                let batch = self
                    .withdrawal_client
                    .get_withdrawals(
                        &blob_id,
                        L1BatchNumber(
                            u32::try_from(number).context("Invalid withdrawal batch number")?,
                        ),
                    )
                    .await?;
                self.master_connection_pool
                    .connection_tagged("withdrawal import")
                    .await?
                    .via_withdrawal_dal()
                    .import_complete_batch(&wallet, &proof_tx_id, &batch)
                    .await
            }
            .await;
            if let Err(error) = imported {
                tracing::warn!(
                    batch_number = number,
                    "Withdrawal batch remains ineligible: {error:#}"
                );
            }
        }
        Ok(())
    }

    async fn parse_bridge_withdrawal(&self, tx: Transaction) -> anyhow::Result<Vec<L1Withdrawal>> {
        let mut parser = MessageParser::new(self.withdrawal_client.network);
        let messages = parser.parse_bridge_transaction(
            &mut TransactionWithMetadata {
                tx,
                output_vout: None,
                tx_index: 0,
            },
            0,
            &self.get_system_wallets().await?,
        );
        anyhow::ensure!(
            messages.len() == 1,
            "Expected exactly one withdrawal message"
        );
        match messages.into_iter().next() {
            Some(FullInscriptionMessage::BridgeWithdrawal(message)) => {
                Ok(message.input.withdrawals)
            }
            _ => anyhow::bail!("Transaction does not contain a complete withdrawal message"),
        }
    }

    async fn get_system_wallets(&self) -> anyhow::Result<SystemWallets> {
        let mut storage = self.master_connection_pool.connection().await?;
        let height = storage
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await?;
        let wallets = storage
            .via_wallet_dal()
            .get_system_wallets_raw(
                i64::try_from(height)
                    .context("Withdrawal observation cursor exceeds storage range")?,
            )
            .await?
            .context("System wallets unavailable at withdrawal observation cursor")?;
        SystemWallets::try_from(wallets)
    }
}

#[async_trait]
impl ISession for WithdrawalSession {
    async fn prepare_session(&self) -> anyhow::Result<()> {
        self.prepare_withdrawal_session().await
    }

    async fn session(&self) -> anyhow::Result<Option<SessionOperation>> {
        let config = TransactionBuilderConfig::withdrawal(self.get_system_wallets().await?.bridge);
        let wallet = config.bridge_address.script_pubkey();
        let requests = self
            .master_connection_pool
            .connection_tagged("withdrawal selection")
            .await?
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet.as_bytes(), 660, WITHDRAWAL_LIMIT)
            .await?;
        if requests.is_empty() {
            return Ok(None);
        }
        let excluded_inputs = self
            .master_connection_pool
            .connection_tagged("withdrawal held inputs")
            .await?
            .via_withdrawal_dal()
            .held_withdrawal_inputs(wallet.as_bytes())
            .await?;
        let transactions = self
            .transaction_builder
            .build_transaction_with_op_return(
                Self::requested_outputs(&requests)?,
                config,
                &excluded_inputs,
            )
            .await?;
        let Some(transaction) = transactions.into_iter().next() else {
            return Ok(None);
        };
        let messages = self.transaction_builder.get_tr_sighashes(&transaction)?;
        let client = self.transaction_builder.utxo_manager.get_btc_client();
        let mut parents = Vec::with_capacity(transaction.utxos.len());
        let mut seen = HashSet::with_capacity(transaction.utxos.len());
        for (outpoint, _) in &transaction.utxos {
            if seen.insert(outpoint.txid) {
                let parent = client
                    .get_transaction(&outpoint.txid)
                    .await
                    .with_context(|| {
                        format!("Required proposal parent {} unavailable", outpoint.txid)
                    })?;
                anyhow::ensure!(
                    parent.compute_txid() == outpoint.txid,
                    "Proposal parent identity mismatch"
                );
                parents.push(parent);
            }
        }
        Ok(Some(SessionOperation::Withdrawal(
            transaction,
            messages,
            parents,
        )))
    }

    async fn is_session_in_progress(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        Ok(!self.is_bridge_session_already_processed(session_op).await?)
    }

    async fn verify_message(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        self.authorize_withdrawal(session_op).await?;
        Ok(true)
    }

    async fn before_process_session(&self, session_op: &SessionOperation) -> anyhow::Result<bool> {
        self.is_session_in_progress(session_op).await
    }

    async fn before_broadcast_final_transaction(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let wallet = self.wallet_script().await?;
        Ok(!self
            .master_connection_pool
            .connection_tagged("withdrawal rebroadcast inclusion")
            .await?
            .via_withdrawal_dal()
            .observed_withdrawal_transaction_has_canonical_inclusion(
                &wallet,
                session_op.get_unsigned_bridge_tx().txid.as_byte_array(),
            )
            .await?)
    }

    async fn after_broadcast_final_transaction(
        &self,
        transaction: &Transaction,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let candidate = session_op.get_unsigned_bridge_tx();
        anyhow::ensure!(
            transaction.compute_txid() == candidate.txid,
            "Broadcast transaction identity differs from authorization"
        );
        let payments = self.parse_bridge_withdrawal(transaction.clone()).await?;
        let observation = WithdrawalObservation {
            transaction: transaction.clone(),
            prevouts: candidate.utxos,
            withdrawals: payments
                .into_iter()
                .map(|payment| ObservedWithdrawal {
                    vout: payment.vout,
                    reference: payment.l2_meta.l2_id,
                    script_pubkey: payment.receiver.script_pubkey(),
                    amount: payment.value,
                })
                .collect(),
            inclusion: None,
        };
        let wallet = self.wallet_script().await?;
        self.master_connection_pool
            .connection_tagged("withdrawal broadcast observation")
            .await?
            .via_withdrawal_dal()
            .record_withdrawal_observation(&wallet, &observation)
            .await?;
        self.transaction_builder
            .utxo_manager_insert_transaction(transaction.clone())
            .await;
        Ok(true)
    }

    async fn is_bridge_session_already_processed(
        &self,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let wallet = self.wallet_script().await?;
        self.master_connection_pool
            .connection_tagged("withdrawal observation lookup")
            .await?
            .via_withdrawal_dal()
            .observed_withdrawal_transaction_exists(
                &wallet,
                session_op.get_unsigned_bridge_tx().txid.as_byte_array(),
            )
            .await
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
