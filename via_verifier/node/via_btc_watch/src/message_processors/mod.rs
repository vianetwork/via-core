use std::str::FromStr;

use bitcoin::BlockHash;
pub(crate) use governance_upgrade::GovernanceUpgradesEventProcessor;
pub(crate) use l1_to_l2::L1ToL2MessageProcessor;
pub(crate) use system_wallet::SystemWalletProcessor;
pub(crate) use verifier::VerifierMessageProcessor;
use via_btc_client::{
    indexer::{BitcoinInscriptionIndexer, BitcoinTxLocatorStore},
    types::{BitcoinTxLocator, BitcoinTxid, FullInscriptionMessage, IndexerError},
};
use via_verifier_dal::{Connection, DalError, Verifier, VerifierDal};
pub(crate) use withdrawal::WithdrawalProcessor;
use zksync_types::H256;

mod governance_upgrade;
mod l1_to_l2;
mod system_wallet;
mod verifier;
mod withdrawal;

#[derive(Debug, thiserror::Error)]
pub(super) enum MessageProcessorError {
    #[error("internal processing error: {0:?}")]
    Internal(#[from] anyhow::Error),
    #[error("database error: {0}")]
    DatabaseError(String),
}

impl From<IndexerError> for MessageProcessorError {
    fn from(err: IndexerError) -> Self {
        MessageProcessorError::Internal(err.into())
    }
}

impl From<DalError> for MessageProcessorError {
    fn from(err: DalError) -> Self {
        MessageProcessorError::DatabaseError(err.to_string())
    }
}

#[async_trait::async_trait]
pub(super) trait MessageProcessor: 'static + std::fmt::Debug + Send + Sync {
    async fn process_messages(
        &mut self, storage: &mut Connection<'_, Verifier>, msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError>;
}

pub(crate) fn convert_txid_to_h256(txid: BitcoinTxid) -> H256 {
    let mut tx_id_bytes = txid.as_raw_hash()[..].to_vec();
    tx_id_bytes.reverse();
    H256::from_slice(&tx_id_bytes)
}

pub(crate) struct DbBitcoinTxLocatorStore<'s, 'a> {
    storage: &'s mut Connection<'a, Verifier>,
}

impl<'s, 'a> DbBitcoinTxLocatorStore<'s, 'a> {
    pub(crate) fn new(storage: &'s mut Connection<'a, Verifier>) -> Self {
        Self { storage }
    }
}

#[async_trait::async_trait]
impl BitcoinTxLocatorStore for DbBitcoinTxLocatorStore<'_, '_> {
    async fn insert_tx_locator(&mut self, locator: &BitcoinTxLocator) -> anyhow::Result<()> {
        self.storage
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

    async fn get_tx_locator(&mut self, txid: &BitcoinTxid) -> anyhow::Result<Option<BitcoinTxLocator>> {
        Ok(load_tx_locator(self.storage, txid).await?)
    }
}

pub(crate) async fn load_tx_locator(
    storage: &mut Connection<'_, Verifier>, txid: &BitcoinTxid,
) -> Result<Option<BitcoinTxLocator>, MessageProcessorError> {
    let Some(locator) = storage.via_btc_tx_locator_dal().get_tx_locator(&txid.to_string()).await? else {
        return Ok(None);
    };

    let block_hash = BlockHash::from_str(&locator.l1_block_hash).map_err(|err| {
        MessageProcessorError::Internal(anyhow::anyhow!(
            "invalid stored Bitcoin block hash {} for tx {}: {}",
            locator.l1_block_hash,
            txid,
            err
        ))
    })?;

    Ok(Some(BitcoinTxLocator {
        txid: *txid,
        block_height: locator.l1_block_number as u32,
        block_hash,
        tx_index: locator.tx_index.map(|index| index as usize),
    }))
}
