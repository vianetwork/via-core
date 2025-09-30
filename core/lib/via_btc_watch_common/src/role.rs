use crate::dal::{IndexerMetaDal, WalletsDal};
use crate::orchestrator::{PreFut, ProcFut};
use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};

/// Role-specific hooks that the orchestrator can call.
pub trait RoleHooks<S>: Send + Sync + 'static
where
    S: IndexerMetaDal + WalletsDal + Send,
{
    /// Optional pre-iteration hook. Default no-op.
    fn pre_iteration(&self, _storage: &mut S) -> PreFut {
        Box::pin(async { Ok(()) })
    }

    /// Apply role-specific processing to messages.
    fn process_messages(
        &self,
        storage: &mut S,
        messages: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> ProcFut;
}


