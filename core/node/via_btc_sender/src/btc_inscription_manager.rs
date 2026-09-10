use std::{collections::HashMap, str::FromStr};

use anyhow::{Context, Result};
use bincode::serialize;
use bitcoin::{hashes::Hash, BlockHash};
use tokio::sync::watch;
use via_btc_client::{
    inscriber::Inscriber,
    traits::Serializable,
    types::{BitcoinTxLocator, InscriptionMessage},
};
use zksync_config::ViaBtcSenderConfig;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_shared_metrics::BlockL1Stage;
use zksync_types::{
    btc_inscription_operations::ViaBtcInscriptionRequestType,
    via_btc_sender::{ViaBtcInscriptionRequest, ViaBtcInscriptionRequestHistory},
    via_wallet::SystemWallets,
};

use crate::metrics::METRICS;

#[derive(Debug)]
pub struct ViaBtcInscriptionManager {
    inscriber: Inscriber,
    config: ViaBtcSenderConfig,
    pool: ConnectionPool<Core>,
}

impl ViaBtcInscriptionManager {
    pub async fn new(
        inscriber: Inscriber, pool: ConnectionPool<Core>, config: ViaBtcSenderConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self { inscriber, config, pool })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.poll_interval());
        let pool = self.pool.clone();

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            let mut storage = pool.connection_tagged("via_btc_sender").await?;

            match self.loop_iteration(&mut storage).await {
                Ok(()) => {}
                Err(err) => {
                    METRICS.manager_errors.inc();
                    tracing::error!("Failed to process btc_sender_inscription_manager: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, btc_sender is shutting down");
        Ok(())
    }

    async fn loop_iteration(&mut self, storage: &mut Connection<'_, Core>) -> Result<(), anyhow::Error> {
        if storage.via_l1_block_dal().has_reorg_in_progress().await?.is_some() {
            return Ok(());
        }

        self.update_inscription_status(storage).await?;
        self.send_new_inscription_txs(storage).await?;
        Ok(())
    }

    async fn update_inscription_status(&mut self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
        let inflight_inscriptions_ids = storage.btc_sender_dal().list_inflight_inscription_ids().await?;

        METRICS.inflight_inscriptions.set(inflight_inscriptions_ids.len());

        let mut report_blocked_l1_batch_inscription: Option<u32> = None;
        let mut inflight_histories = Vec::with_capacity(inflight_inscriptions_ids.len());

        for inscription_id in &inflight_inscriptions_ids {
            if let Some(last_inscription_history) =
                storage.btc_sender_dal().get_last_inscription_request_history(*inscription_id).await?
            {
                inflight_histories.push((*inscription_id, last_inscription_history));
            }
        }

        if let Some(from_block_height) =
            inflight_histories.iter().map(|(_, history)| history.sent_at_block.max(0) as u64).min()
        {
            self.inscriber.sync_context_with_blockchain_from_block(from_block_height).await?;
        } else {
            self.inscriber.sync_context_with_blockchain().await?;
        }

        let last_processed_l1_block = storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?;

        let Some(wallets_map) = storage.via_wallet_dal().get_system_wallets_raw(last_processed_l1_block as i64).await?
        else {
            anyhow::bail!("System wallets not found");
        };

        self.validate_inscriber_address(wallets_map)?;
        METRICS.track_block_numbers(storage).await;

        for (inscription_id, last_inscription_history) in inflight_histories {
            let current_block = self.inscriber.get_client().await.fetch_block_height().await?;
            let is_confirmed =
                self.check_reveal_tx_confirmation(storage, &last_inscription_history, current_block).await?;

            if is_confirmed {
                let inscription =
                    storage.btc_sender_dal().confirm_inscription(inscription_id, last_inscription_history.id).await?;
                tracing::info!("Inscription confirmed {reveal_tx}", reveal_tx = last_inscription_history.reveal_tx_id,);

                METRICS
                    .track_btc_tx_metrics(
                        storage,
                        BlockL1Stage::Mined,
                        vec![(inscription.id as u32, ViaBtcInscriptionRequestType::from(inscription.request_type))],
                    )
                    .await;

                METRICS.track_inscription_confirmation(last_inscription_history.created_at);
            } else {
                if last_inscription_history.sent_at_block + self.config.stuck_inscription_block_number() as i64
                    > current_block as i64
                {
                    continue;
                }

                if report_blocked_l1_batch_inscription.is_none() {
                    let l1_batch_number = storage
                        .via_blocks_dal()
                        .get_first_stuck_l1_batch_number_inscription_request(
                            self.config.stuck_inscription_block_number(),
                            current_block,
                        )
                        .await?;

                    METRICS.report_blocked_l1_batch_inscription.set(l1_batch_number as usize);

                    report_blocked_l1_batch_inscription = Some(l1_batch_number);

                    tracing::debug!(
                        "Inscription {} stuck for more than {} block.",
                        last_inscription_history.reveal_tx_id,
                        self.config.stuck_inscription_block_number()
                    );
                }
            }
        }

        let balance = self.inscriber.get_balance().await?;
        METRICS.btc_sender_account_balance[&self.config.wallet_address.clone()].set(balance as usize);

        Ok(())
    }

    async fn check_reveal_tx_confirmation(
        &self, storage: &mut Connection<'_, Core>, history: &ViaBtcInscriptionRequestHistory, current_block: u64,
    ) -> anyhow::Result<bool> {
        let client = self.inscriber.get_client().await;
        let from_block = history.sent_at_block.max(0) as u64;
        let locator = match load_tx_locator(storage, &history.reveal_tx_id).await? {
            Some(locator) => locator,
            None => {
                let Some(locator) =
                    client.find_transaction_locator(&history.reveal_tx_id, from_block, current_block).await?
                else {
                    return Ok(false);
                };

                insert_tx_locator(storage, &locator).await?;
                locator
            }
        };

        let confirmation_result = client
            .check_tx_confirmation_in_block(&history.reveal_tx_id, &locator.block_hash, self.config.block_confirmations)
            .await;

        match confirmation_result {
            Ok(true) => Ok(true),
            Ok(false) if !self.is_tx_locator_stale(&locator, current_block).await? => Ok(false),
            Ok(false) => {
                let Some(locator) =
                    client.find_transaction_locator(&history.reveal_tx_id, from_block, current_block).await?
                else {
                    return Ok(false);
                };

                insert_tx_locator(storage, &locator).await?;
                Ok(client
                    .check_tx_confirmation_in_block(
                        &history.reveal_tx_id,
                        &locator.block_hash,
                        self.config.block_confirmations,
                    )
                    .await?)
            }
            Err(err) => {
                tracing::warn!(
                    "Failed to check reveal tx {} with stored Bitcoin locator {}: {}",
                    history.reveal_tx_id,
                    locator.block_hash,
                    err
                );

                let Some(locator) =
                    client.find_transaction_locator(&history.reveal_tx_id, from_block, current_block).await?
                else {
                    return Ok(false);
                };

                insert_tx_locator(storage, &locator).await?;
                Ok(client
                    .check_tx_confirmation_in_block(
                        &history.reveal_tx_id,
                        &locator.block_hash,
                        self.config.block_confirmations,
                    )
                    .await?)
            }
        }
    }

    async fn is_tx_locator_stale(&self, locator: &BitcoinTxLocator, current_block: u64) -> anyhow::Result<bool> {
        if u64::from(locator.block_height) > current_block {
            return Ok(true);
        }

        let client = self.inscriber.get_client().await;
        match client.fetch_block(u128::from(locator.block_height)).await {
            Ok(active_block) => Ok(active_block.block_hash() != locator.block_hash),
            Err(err) => {
                tracing::warn!(
                    "Failed to load active Bitcoin block {} while validating stored locator for tx {}: {}",
                    locator.block_height,
                    locator.txid,
                    err
                );
                Ok(true)
            }
        }
    }

    async fn send_new_inscription_txs(&mut self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
        let number_inflight_txs = storage.btc_sender_dal().list_inflight_inscription_ids().await?.len();

        tracing::debug!("Inflight inscriptions: {count}", count = number_inflight_txs);

        let number_of_available_slots_for_inscription_txs =
            self.config.max_txs_in_flight.saturating_sub(number_inflight_txs as i64);

        tracing::debug!(
            "Available slots to process inscriptions: {count}",
            count = number_of_available_slots_for_inscription_txs
        );

        if number_of_available_slots_for_inscription_txs > 0 {
            let list_new_inscription_request = storage
                .btc_sender_dal()
                .list_new_inscription_request(number_of_available_slots_for_inscription_txs)
                .await?;

            for inscription in list_new_inscription_request {
                self.send_inscription_tx(storage, &inscription).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn send_inscription_tx(
        &mut self, storage: &mut Connection<'_, Core>, tx: &ViaBtcInscriptionRequest,
    ) -> anyhow::Result<()> {
        let sent_at_block = self.inscriber.get_client().await.fetch_block_height().await? as i64;

        let input = InscriptionMessage::from_bytes(&tx.inscription_message.clone().unwrap_or_default());

        let latency = METRICS.broadcast_time.start();
        let inscribe_info = match self.inscriber.inscribe(input).await {
            Ok(info) => info,
            Err(e) => {
                METRICS.l1_transient_errors.inc();
                return Err(anyhow::anyhow!(e));
            }
        };

        latency.observe();

        let signed_commit_tx =
            serialize(&inscribe_info.final_commit_tx.tx).with_context(|| "Error serializing the commit tx")?;

        let signed_reveal_tx =
            serialize(&inscribe_info.final_reveal_tx.tx).with_context(|| "Error serializing the reveal tx")?;

        let actual_fees =
            inscribe_info.reveal_tx_output_info._reveal_fee + inscribe_info.commit_tx_output_info.commit_tx_fee;

        tracing::info!("New inscription created {}", inscribe_info.final_reveal_tx.txid,);

        storage
            .btc_sender_dal()
            .insert_inscription_request_history(
                &inscribe_info.final_commit_tx.txid.as_raw_hash().to_byte_array(),
                &inscribe_info.final_reveal_tx.txid.as_raw_hash().to_byte_array(),
                tx.id,
                &signed_commit_tx,
                &signed_reveal_tx,
                actual_fees.to_sat() as i64,
                sent_at_block,
            )
            .await?;
        METRICS.pending_inscription_requests.dec_by(1);
        Ok(())
    }

    fn validate_inscriber_address(&self, wallets_map: HashMap<String, String>) -> anyhow::Result<()> {
        let wallets = SystemWallets::try_from(wallets_map)?;

        let inscriber_address = self.inscriber.inscriber_address()?;
        if wallets.sequencer != inscriber_address {
            anyhow::bail!(
                "BTC sender inscriber wallets is not valid, expected {} found {}",
                wallets.sequencer,
                inscriber_address
            )
        }
        Ok(())
    }
}

async fn load_tx_locator(
    storage: &mut Connection<'_, Core>, txid: &bitcoin::Txid,
) -> anyhow::Result<Option<BitcoinTxLocator>> {
    let Some(locator) = storage.via_btc_tx_locator_dal().get_tx_locator(&txid.to_string()).await? else {
        return Ok(None);
    };

    let block_hash = BlockHash::from_str(&locator.l1_block_hash)
        .with_context(|| format!("invalid Bitcoin block hash {} for tx {}", locator.l1_block_hash, txid))?;

    Ok(Some(BitcoinTxLocator {
        txid: *txid,
        block_height: locator.l1_block_number as u32,
        block_hash,
        tx_index: locator.tx_index.map(|index| index as usize),
    }))
}

async fn insert_tx_locator(storage: &mut Connection<'_, Core>, locator: &BitcoinTxLocator) -> anyhow::Result<()> {
    storage
        .via_btc_tx_locator_dal()
        .insert_tx_locator(
            &locator.txid.to_string(),
            i64::from(locator.block_height),
            &locator.block_hash.to_string(),
            locator.tx_index.map(|index| index as i32),
        )
        .await?;
    Ok(())
}
