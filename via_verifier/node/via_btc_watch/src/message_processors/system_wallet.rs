use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, MessageParser},
    traits::BitcoinOps,
    types::{
        BitcoinAddress, FullInscriptionMessage, UpdateBridge, UpdateGovernance, UpdateSequencer,
    },
};
use via_verifier_dal::{Connection, Verifier, VerifierDal};
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
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        let mut l1_block_number: Option<u32> = None;

        for msg in FullInscriptionMessage::sort_messages(msgs) {
            let l1_block_number_opt = match msg {
                FullInscriptionMessage::UpdateGovernance(m) => {
                    self.handle_update_governance(storage, m).await?
                }
                FullInscriptionMessage::UpdateSequencer(m) => {
                    self.handle_update_sequencer(storage, m).await?
                }
                FullInscriptionMessage::UpdateBridge(m) => {
                    self.handle_update_bridge_proposal(storage, m).await?
                }
                _ => None,
            };

            // Keep the smallest (earliest) block number
            match (l1_block_number, l1_block_number_opt) {
                (Some(current), Some(new)) if new < current => l1_block_number = Some(new),
                (None, Some(new)) => l1_block_number = Some(new),
                _ => {}
            }
        }

        Ok(l1_block_number)
    }
}

impl SystemWalletProcessor {
    async fn handle_update_bridge_proposal(
        &self,
        storage: &mut Connection<'_, Verifier>,
        update_bridge_msg: UpdateBridge,
    ) -> Result<Option<u32>, MessageProcessorError> {
        let proposal_tx_id = update_bridge_msg.input.proposal_tx_id;

        let proposal_tx = match self.btc_client.get_transaction(&proposal_tx_id).await {
            Ok(proposal_tx) => proposal_tx,
            Err(err) => {
                tracing::warn!(
                    "Failed to fetch update bridge proposal transaction: {}, error {}",
                    proposal_tx_id,
                    err
                );
                return Ok(None);
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
                    let system_wallets_map = match storage
                        .via_wallet_dal()
                        .get_system_wallets_raw(update_bridge_msg.common.block_height as i64)
                        .await?
                    {
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
                            return Ok(None);
                        }
                    };

                    // Skip if bridge already registered
                    if system_wallets.bridge == new_bridge_address {
                        tracing::info!("Bridge wallet already exists, skipping");
                        return Ok(None);
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
                        .insert_wallets(
                            &wallets_details,
                            update_bridge_msg.common.block_height as i64,
                        )
                        .await?;

                    tracing::info!("New bridge address updated: {:?}", &wallets_details);

                    return Ok(Some(update_bridge_msg.common.block_height));
                }
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    async fn handle_update_sequencer(
        &self,
        storage: &mut Connection<'_, Verifier>,
        update_sequencer_msg: UpdateSequencer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        tracing::info!("Received UpdateSequencer message");

        let system_wallets_map = match storage
            .via_wallet_dal()
            .get_system_wallets_raw(update_sequencer_msg.common.block_height as i64)
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
            return Ok(None);
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
            .insert_wallets(
                &wallets_details,
                update_sequencer_msg.common.block_height as i64,
            )
            .await?;

        tracing::info!("New sequencer address updated: {:?}", &wallets_details);

        Ok(Some(update_sequencer_msg.common.block_height))
    }

    async fn handle_update_governance(
        &self,
        storage: &mut Connection<'_, Verifier>,
        update_governance_msg: UpdateGovernance,
    ) -> Result<Option<u32>, MessageProcessorError> {
        tracing::info!("Received UpdateGovernance message");

        let system_wallets_map = match storage
            .via_wallet_dal()
            .get_system_wallets_raw(update_governance_msg.common.block_height as i64)
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
            return Ok(None);
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
            .insert_wallets(
                &wallets_details,
                update_governance_msg.common.block_height as i64,
            )
            .await?;

        tracing::info!("New governance address updated: {:?}", &wallets_details);

        Ok(Some(update_governance_msg.common.block_height))
    }
}
