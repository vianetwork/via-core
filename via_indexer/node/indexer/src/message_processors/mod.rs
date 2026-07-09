use std::str::FromStr;

use bitcoin::BlockHash;
pub(crate) use deposit::L1ToL2MessageProcessor;
pub(crate) use system_wallet::SystemWalletProcessor;
use via_btc_client::{
    indexer::{BitcoinInscriptionIndexer, BitcoinTxLocatorStore},
    types::{BitcoinTxLocator, BitcoinTxid, FullInscriptionMessage},
};
use via_indexer_dal::{Connection, Indexer, IndexerDal};
pub(crate) use withdrawal::WithdrawalProcessor;

mod deposit;
mod system_wallet;
mod withdrawal;

#[async_trait::async_trait]
pub(super) trait MessageProcessor: 'static + std::fmt::Debug + Send + Sync {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Indexer>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> anyhow::Result<bool>;
}

pub(crate) struct DbBitcoinTxLocatorStore<'s, 'a> {
    storage: &'s mut Connection<'a, Indexer>,
}

impl<'s, 'a> DbBitcoinTxLocatorStore<'s, 'a> {
    pub(crate) fn new(storage: &'s mut Connection<'a, Indexer>) -> Self {
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

    async fn get_tx_locator(
        &mut self,
        txid: &BitcoinTxid,
    ) -> anyhow::Result<Option<BitcoinTxLocator>> {
        load_tx_locator(self.storage, txid).await
    }
}

pub(crate) async fn load_tx_locator(
    storage: &mut Connection<'_, Indexer>,
    txid: &BitcoinTxid,
) -> anyhow::Result<Option<BitcoinTxLocator>> {
    let Some(locator) = storage
        .via_btc_tx_locator_dal()
        .get_tx_locator(&txid.to_string())
        .await?
    else {
        return Ok(None);
    };

    let block_hash = BlockHash::from_str(&locator.l1_block_hash)?;

    Ok(Some(BitcoinTxLocator {
        txid: *txid,
        block_height: locator.l1_block_number as u32,
        block_hash,
        tx_index: locator.tx_index.map(|index| index as usize),
    }))
}
