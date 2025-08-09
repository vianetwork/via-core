use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, MessageParser},
    traits::BitcoinOps,
    types::{
        BitcoinAddress, FullInscriptionMessage, UpdateBridge, UpdateGovernance, UpdateSequencer,
    },
};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails, WalletInfo, WalletRole};

use crate::message_processors::{MessageProcessor, MessageProcessorError};

#[derive(Debug)]
pub struct SystemWalletProcessor {
    /// BTC client
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
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<bool, MessageProcessorError> {
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
        storage: &mut Connection<'_, Core>,
        update_bridge_msg: UpdateBridge,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<bool, MessageProcessorError> {
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

        for message in messages {
            match message {
                FullInscriptionMessage::UpdateBridgeProposal(update_bridge_msg) => {
                    let system_wallets_map =
                        match storage.via_wallet_dal().get_system_wallets_raw().await? {
                            Some(map) => map,
                            None => Default::default(),
                        };

                    let system_wallets = SystemWallets::try_from(system_wallets_map)?;

                    let new_bridge_address = match update_bridge_msg
                        .input
                        .bridge_musig2_address
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

                    let mut wallets_details = SystemWalletsDetails::default();

                    wallets_details.0.insert(
                        WalletRole::Bridge,
                        WalletInfo {
                            addresses: vec![new_bridge_address.clone()],
                            txid: update_bridge_msg.common.tx_id.clone(),
                        },
                    );

                    let verifier_addresses = update_bridge_msg
                        .input
                        .verifier_p2wpkh_addresses
                        .iter()
                        .map(|addr| addr.clone().assume_checked())
                        .collect::<Vec<BitcoinAddress>>();

                    wallets_details.0.insert(
                        WalletRole::Verifier,
                        WalletInfo {
                            addresses: verifier_addresses.clone(),
                            txid: update_bridge_msg.common.tx_id.clone(),
                        },
                    );

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
        storage: &mut Connection<'_, Core>,
        update_sequencer_msg: UpdateSequencer,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<bool, MessageProcessorError> {
        tracing::info!("Received UpdateSequencer message");
        let system_wallets_map = match storage
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await
            .unwrap()
        {
            Some(map) => map,
            None => Default::default(),
        };

        let system_wallets = SystemWallets::try_from(system_wallets_map)?;

        let new_sequencer_address = update_sequencer_msg.input.address.assume_checked();

        // Skip if sequencer already registered
        if system_wallets.sequencer == new_sequencer_address {
            tracing::info!("Sequencer wallet already exists, skipping");
            return Ok(false);
        }

        let mut wallets_details = SystemWalletsDetails::default();

        wallets_details.0.insert(
            WalletRole::Sequencer,
            WalletInfo {
                addresses: vec![new_sequencer_address.clone()],
                txid: update_sequencer_msg.common.tx_id.clone(),
            },
        );

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
        storage: &mut Connection<'_, Core>,
        update_governance_msg: UpdateGovernance,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<bool, MessageProcessorError> {
        tracing::info!("Received UpdateGovernance message");

        let system_wallets_map = match storage
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await
            .unwrap()
        {
            Some(map) => map,
            None => Default::default(),
        };

        let system_wallets = SystemWallets::try_from(system_wallets_map)?;

        let new_governance_address = update_governance_msg.input.address.assume_checked();

        // Skip if sequencer already registered
        if system_wallets.governance == new_governance_address {
            tracing::info!("Sequencer wallet already exists, skipping");
            return Ok(false);
        }

        let mut wallets_details = SystemWalletsDetails::default();

        wallets_details.0.insert(
            WalletRole::Gov,
            WalletInfo {
                addresses: vec![new_governance_address.clone()],
                txid: update_governance_msg.common.tx_id.clone(),
            },
        );

        storage
            .via_wallet_dal()
            .insert_wallets(&wallets_details)
            .await?;

        indexer.update_system_wallets(None, None, None, Some(new_governance_address));

        tracing::info!("New governance address updated: {:?}", &wallets_details);

        Ok(true)
    }
}
