use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer},
    types::FullInscriptionMessage,
};
use via_verifier_dal::{Connection, DalError, Verifier, VerifierDal};
use zksync_types::via_protocol_upgrade::ViaProtocolUpgrade;

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::{InscriptionStage, METRICS},
};

/// Listens to operation events coming from the governance contract and saves new protocol upgrade proposals to the database.
#[derive(Debug)]
pub struct GovernanceUpgradesEventProcessor {
    /// BTC client
    btc_client: Arc<BitcoinClient>,
    /// upgrade proposal
    upgrade: ViaProtocolUpgrade,
}

impl GovernanceUpgradesEventProcessor {
    pub fn new(btc_client: Arc<BitcoinClient>) -> Self {
        Self { btc_client, upgrade: ViaProtocolUpgrade::default() }
    }
}

// jscpd:ignore-start Each protocol catalogue and system-transaction adapter owns its concrete database transaction.
#[async_trait::async_trait]
impl MessageProcessor for GovernanceUpgradesEventProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        for approval in msgs {
            let FullInscriptionMessage::SystemContractUpgrade(message) = &approval else {
                continue;
            };
            let prepared =
                via_btc_client::migration::prepare_upgrade(&self.btc_client, message).await?;
            let details = &prepared.proposal;
            let version = details.version;
            let hash =
                self.upgrade.get_canonical_tx_hash(version, details.system_contracts.clone())?;
            storage
                .via_protocol_versions_dal()
                .record_via_upgrade_origin(
                    version,
                    message.common.tx_id,
                    &prepared.payload,
                    prepared.inclusion,
                )
                .await
                .map_err(DalError::generalize)?;
            storage
                .via_protocol_versions_dal()
                .save_protocol_version(
                    version,
                    details.bootloader_code_hash.as_bytes(),
                    details.default_account_code_hash.as_bytes(),
                    hash.as_bytes(),
                    details.recursion_scheduler_level_vk_hash.as_bytes(),
                )
                .await
                .map_err(DalError::generalize)?;
            METRICS.inscriptions_processed[&InscriptionStage::Upgrade].set(version.minor as usize);
        }
        Ok(None)
    }
}
// jscpd:ignore-end
