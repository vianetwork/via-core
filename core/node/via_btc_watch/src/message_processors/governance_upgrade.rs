use std::sync::Arc;

use anyhow::Context as _;
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer},
    types::FullInscriptionMessage,
};
use zksync_dal::{Connection, Core, CoreDal, DalError};
use zksync_types::{via_protocol_upgrade::ViaProtocolUpgrade, ProtocolUpgrade};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::{InscriptionStage, METRICS},
};

/// Reads system contract upgrade inscriptions from Bitcoin and saves new protocol upgrade proposals to the database.
#[derive(Debug)]
pub struct GovernanceUpgradesEventProcessor {
    btc_client: Arc<BitcoinClient>,
}

impl GovernanceUpgradesEventProcessor {
    pub fn new(btc_client: Arc<BitcoinClient>) -> Self {
        Self { btc_client }
    }
}
// jscpd:ignore-start Each protocol catalogue and system-transaction adapter owns its concrete database transaction.
#[async_trait::async_trait]
impl MessageProcessor for GovernanceUpgradesEventProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
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
            let governing = storage
                .protocol_versions_dal()
                .latest_via_semantic_version_before(u64::from(message.common.block_height))
                .await
                .map_err(DalError::generalize)?
                .context("upgrade has no historical code configuration")?;
            let base_minor = if version <= governing { version.minor } else { governing.minor };
            let base = storage
                .protocol_versions_dal()
                .get_protocol_version_with_latest_patch(base_minor)
                .await
                .map_err(DalError::generalize)?
                .context("upgrade base version is missing")?;
            let tx = ViaProtocolUpgrade::default()
                .create_protocol_upgrade_tx(version, details.system_contracts.clone())?;
            let upgrade = ProtocolUpgrade {
                version,
                bootloader_code_hash: Some(details.bootloader_code_hash),
                default_account_code_hash: Some(details.default_account_code_hash),
                evm_emulator_code_hash: details.evm_emulator_code_hash,
                tx: Some(tx),
                timestamp: 0,
                verifier_address: None,
                verifier_params: None,
            };
            let new_version =
                base.apply_upgrade(upgrade, Some(details.recursion_scheduler_level_vk_hash));
            storage
                .protocol_versions_dal()
                .record_via_upgrade_origin(
                    version,
                    message.common.tx_id,
                    &prepared.payload,
                    prepared.inclusion,
                )
                .await
                .map_err(DalError::generalize)?;
            storage
                .protocol_versions_dal()
                .save_protocol_version_with_tx(&new_version)
                .await
                .map_err(DalError::generalize)?;
            METRICS.inscriptions_processed[&InscriptionStage::Upgrade].set(version.minor as usize);
        }
        Ok(None)
    }
}
// jscpd:ignore-end
