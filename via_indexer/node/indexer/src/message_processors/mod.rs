pub(crate) use deposit::L1ToL2MessageProcessor;
use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use via_indexer_dal::{Connection, Indexer};
pub(crate) use withdrawal::WithdrawalProcessor;

mod deposit;
mod withdrawal;

#[async_trait::async_trait]
pub(super) trait MessageProcessor: 'static + std::fmt::Debug + Send + Sync {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Indexer>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> anyhow::Result<()>;
}
