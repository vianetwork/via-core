// jscpd:ignore-start Separate node-role adapters own SQL transactions and retain independently tested configuration admission.
use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::{replay::semantic_payload, BitcoinInscriptionIndexer, MessageParser},
    traits::BitcoinOps,
    types::FullInscriptionMessage,
};
use via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD;
use via_reorg::configuration::{admit_rotation, duplicate_roles, RotationAdmission};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::via_wallet::{SystemWalletsDetails, WalletInfo, WalletRole};

use crate::message_processors::{MessageProcessor, MessageProcessorError};

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
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        use FullInscriptionMessage as M;
        let roles = |message: &FullInscriptionMessage| match message {
            M::UpdateGovernance(_) => vec!["Gov"],
            M::UpdateSequencer(_) => vec!["Sequencer"],
            M::UpdateBridge(_) => vec!["Bridge", "Verifier"],
            _ => vec![],
        };
        let duplicates = duplicate_roles(msgs.iter().flat_map(|message| {
            let common = message.common();
            roles(message).into_iter().map(move |role| {
                ((common.block_height, role), (common.tx_id, common.message_index))
            })
        }));
        let mut changed = None;
        for message in FullInscriptionMessage::sort_messages(msgs) {
            let affected_roles = roles(&message);
            if affected_roles.is_empty() {
                continue;
            }
            let rejected_roles = affected_roles
                .iter()
                .map(|role| role.parse::<WalletRole>())
                .collect::<Result<Vec<_>, _>>()?;
            let common = message.common().clone();
            let inclusion =
                common.inclusion().map_err(|err| MessageProcessorError::Internal(err.into()))?;
            let mut payload = semantic_payload(&message)
                .map_err(|err| MessageProcessorError::Internal(err.into()))?;
            let mut details = SystemWalletsDetails::default();
            let mut updates = Vec::new();
            let mut decision = if affected_roles
                .iter()
                .any(|role| duplicates.contains(&(common.block_height, *role)))
            {
                "duplicate_role"
            } else {
                "accepted"
            };
            match message {
                M::UpdateGovernance(message) => {
                    let Ok(address)=message.input.address.require_network(self.btc_client.get_network()) else {
                        storage.via_wallet_dal().record_wallet_rejection(common.tx_id,&rejected_roles,inclusion,&payload).await?;
                        continue;
                    };
                    updates.push((WalletRole::Gov,vec![address]));
                }
                M::UpdateSequencer(message) => {
                    let Ok(address)=message.input.address.require_network(self.btc_client.get_network()) else {
                        storage.via_wallet_dal().record_wallet_rejection(common.tx_id,&rejected_roles,inclusion,&payload).await?;
                        continue;
                    };
                    updates.push((WalletRole::Sequencer,vec![address]));
                }
                M::UpdateBridge(message) => {
                    let transaction = self.btc_client.get_transaction(&message.input.proposal_tx_id).await
                        .map_err(|source| MessageProcessorError::ConfigurationNotReady { approval:common.tx_id,
                            proposal:message.input.proposal_tx_id,source:source.into() })?;
                    let proposals: Vec<_> = MessageParser::new(self.btc_client.get_network())
                        .parse_system_transaction(&transaction,common.block_height,None).into_iter()
                        .filter(|message| matches!(message,M::UpdateBridgeProposal(_))).collect();
                    if proposals.len()!=1 {
                        payload["proposals"]=serde_json::Value::Array(proposals.iter().map(semantic_payload).collect::<Result<_,_>>().map_err(anyhow::Error::from)?);
                        storage.via_wallet_dal().record_wallet_rejection(common.tx_id,&rejected_roles,inclusion,&payload).await?;
                        continue;
                    }
                    payload["proposal"] = semantic_payload(&proposals[0]).map_err(|err| MessageProcessorError::Internal(err.into()))?;
                    let M::UpdateBridgeProposal(proposal) = &proposals[0] else { unreachable!() };
                    let bridge=proposal.input.bridge_musig2_address.clone().require_network(self.btc_client.get_network());
                    let verifiers=proposal.input.verifier_p2wpkh_addresses.iter().cloned()
                        .map(|address|address.require_network(self.btc_client.get_network())).collect::<Result<Vec<_>,_>>();
                    let (Ok(bridge),Ok(verifiers))=(bridge,verifiers) else {
                        storage.via_wallet_dal().record_wallet_rejection(common.tx_id,&rejected_roles,inclusion,&payload).await?;
                        continue;
                    };
                    if verifiers.is_empty() {
                        storage.via_wallet_dal().record_wallet_rejection(common.tx_id,&rejected_roles,inclusion,&payload).await?;
                        continue;
                    }
                    updates.push((WalletRole::Bridge,vec![bridge]));
                    updates.push((WalletRole::Verifier,verifiers));
                    if decision=="accepted" {
                        let pending = storage.via_votes_dal().prefix_has_unfinalized_proof(
                            [i64::from(inclusion.block.height)+1,0,0],BATCH_FINALIZATION_THRESHOLD,
                        ).await?;
                        if admit_rotation(true,pending)==RotationAdmission::RejectedEarlierUnfinalizedProof {
                            decision="earlier_unfinalized_proof";
                        }
                    }
                }
                _ => unreachable!(),
            }
            for (role, addresses) in updates {
                details.0.insert(role, WalletInfo { addresses, txid: common.tx_id });
            }
            storage
                .via_wallet_dal()
                .record_wallet_update(&details, inclusion, &payload, decision)
                .await?;
            if decision == "accepted" {
                changed =
                    Some(changed.map_or(common.block_height, |height: u32| {
                        height.min(common.block_height)
                    }));
            }
        }
        Ok(changed)
    }
}
// jscpd:ignore-end
