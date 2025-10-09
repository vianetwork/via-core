use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};

use crate::{
    dal::{IndexerMetaDal, WalletsDal},
    orchestrator::{PreFut, ProcFut},
};

pub trait RoleHooks<S>: Send + Sync + 'static
where
    S: IndexerMetaDal + WalletsDal + Send,
{
    fn pre_iteration(&self, _storage: &mut S) -> PreFut {
        Box::pin(async { Ok(()) })
    }

    fn process_messages(
        &self,
        storage: &mut S,
        messages: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> ProcFut;
}
