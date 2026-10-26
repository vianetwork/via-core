use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, MessageParser},
    traits::BitcoinOps,
    types::{FullInscriptionMessage, UpdateBridge, UpdateGovernance, UpdateSequencer},
};
use via_indexer_dal::{Connection, Indexer, IndexerDal};
use zksync_types::via_wallet::{SystemWallets, WalletRole};

use crate::message_processors::MessageProcessor;
use via_btc_client::indexer::configuration::{
    ConfigurationInput, project_wallets, project_bridge_wallets,
};

#[derive(Debug)]
pub struct SystemWalletProcessor {
    btc_client: Arc<BitcoinClient>,
}

impl SystemWalletProcessor {
    pub fn new(btc_client: Arc<BitcoinClient>) -> Self {
        Self { btc_client }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for SystemWalletProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Indexer>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> anyhow::Result<bool> {
        let mut wallets_updated = false;

        let msgs = FullInscriptionMessage::sort_messages(msgs);

        for msg in msgs {
            match msg {
                FullInscriptionMessage::UpdateGovernance(msg) => {
                    let updated = self.handle_update_governance(storage, msg, indexer).await?;
                    // Make sure to not override the wallets_updated if "wallets_updated" it's already true.
                    if updated {
                        wallets_updated = updated;
                    }
                }
                FullInscriptionMessage::UpdateSequencer(msg) => {
                    let updated = self.handle_update_sequencer(storage, msg, indexer).await?;
                    if updated {
                        wallets_updated = updated;
                    }
                }
                FullInscriptionMessage::UpdateBridge(msg) => {
                    let updated = self
                        .handle_update_bridge_proposal(storage, msg, indexer)
                        .await?;
                    if updated {
                        wallets_updated = updated;
                    }
                }

                _ => {}
            }
        }
        Ok(wallets_updated)
    }
}

impl SystemWalletProcessor {
    async fn handle_update_bridge_proposal(
        &self,
        storage: &mut Connection<'_, Indexer>,
        update_bridge_msg: UpdateBridge,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> anyhow::Result<bool> {
        let proposal_tx_id = update_bridge_msg.input.proposal_tx_id;

        let proposal_tx = match self.btc_client.get_transaction(&proposal_tx_id).await {
            Ok(proposal_tx) => proposal_tx,
            Err(err) => {
                tracing::warn!(
                    "Failed to fetch update bridge proposal transaction: {}, error {}",
                    proposal_tx_id,
                    err
                );
                return Ok(false);
            }
        };

        let mut message_parser = MessageParser::new(self.btc_client.get_network());

        let messages = message_parser.parse_system_transaction(
            &proposal_tx,
            update_bridge_msg.common.block_height,
            None,
        );

        if let Some(message) = messages.into_iter().next() {
            match message {
                FullInscriptionMessage::UpdateBridgeProposal(update_bridge_msg) => {
                    let system_wallets_map = storage
                        .via_wallet_dal()
                        .get_system_wallets_raw()
                        .await?
                        .unwrap_or_default();

                    let system_wallets = SystemWallets::try_from(system_wallets_map)?;

                    let new_bridge_address = match update_bridge_msg
                        .input
                        .bridge_musig2_address
                        .clone()
                        .require_network(self.btc_client.get_network())
                    {
                        Ok(address) => address,
                        Err(err) => {
                            tracing::error!("Failed to parse bridge address: {}", err);
                            return Ok(false);
                        }
                    };

                    // Skip if bridge already registered
                    if system_wallets.bridge == new_bridge_address {
                        tracing::info!("Bridge wallet already exists, skipping");
                        return Ok(false);
                    }

                    let wallets_details = project_bridge_wallets(&update_bridge_msg.input, update_bridge_msg.common.tx_id, self.btc_client.get_network())?;
                    let verifier_addresses = wallets_details.0[&WalletRole::Verifier].addresses.clone();
                    storage
                        .via_wallet_dal()
                        .insert_wallets(&wallets_details)
                        .await?;

                    indexer.update_system_wallets(
                        None,
                        Some(new_bridge_address),
                        Some(verifier_addresses),
                        None,
                    );

                    tracing::info!("New bridge address updated: {:?}", &wallets_details);

                    return Ok(true);
                }
                _ => return Ok(false),
            }
        }
        Ok(false)
    }

    async fn handle_update_sequencer(
        &self,
        storage: &mut Connection<'_, Indexer>,
        update_sequencer_msg: UpdateSequencer,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> anyhow::Result<bool> {
        tracing::info!("Received UpdateSequencer message");

        let system_wallets_map =
            storage.via_wallet_dal().get_system_wallets_raw().await.unwrap().unwrap_or_default();

        let system_wallets = SystemWallets::try_from(system_wallets_map)?;

        let new_sequencer_address = update_sequencer_msg.input.address.clone().assume_checked();

        // Skip if sequencer already registered
        if system_wallets.sequencer == new_sequencer_address {
            tracing::info!("Sequencer wallet already exists, skipping");
            return Ok(false);
        }

        let wallets_details = project_wallets(
            &ConfigurationInput::Sequencer(update_sequencer_msg.input),
            update_sequencer_msg.common.tx_id,
            self.btc_client.get_network(),
        )?;

        storage
            .via_wallet_dal()
            .insert_wallets(&wallets_details)
            .await?;

        indexer.update_system_wallets(Some(new_sequencer_address), None, None, None);

        tracing::info!("New sequencer address updated: {:?}", &wallets_details);

        Ok(true)
    }

    async fn handle_update_governance(
        &self,
        storage: &mut Connection<'_, Indexer>,
        update_governance_msg: UpdateGovernance,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> anyhow::Result<bool> {
        tracing::info!("Received UpdateGovernance message");

        let system_wallets_map =
            storage.via_wallet_dal().get_system_wallets_raw().await.unwrap().unwrap_or_default();

        let system_wallets = SystemWallets::try_from(system_wallets_map)?;

        let new_governance_address = update_governance_msg.input.address.clone().assume_checked();

        // Skip if sequencer already registered
        if system_wallets.governance == new_governance_address {
            tracing::info!("Sequencer wallet already exists, skipping");
            return Ok(false);
        }

        let wallets_details = project_wallets(
            &ConfigurationInput::Governance(update_governance_msg.input),
            update_governance_msg.common.tx_id,
            self.btc_client.get_network(),
        )?;

        storage
            .via_wallet_dal()
            .insert_wallets(&wallets_details)
            .await?;

        indexer.update_system_wallets(None, None, None, Some(new_governance_address));

        tracing::info!("New governance address updated: {:?}", &wallets_details);

        Ok(true)
    }
}
