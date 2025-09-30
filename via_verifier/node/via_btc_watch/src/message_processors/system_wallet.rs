// Re-export the shared processor for compatibility with existing module structure
pub use via_btc_watch_common::system_wallet::SystemWalletProcessor;
use via_btc_watch_common::dal::WalletsDal;
use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use via_verifier_dal::{Connection, Verifier, VerifierDal};

use crate::message_processors::{MessageProcessor, MessageProcessorError};

struct VerifierWalletsConn<'r, 'c> {
    inner: &'r mut Connection<'c, Verifier>,
}

#[async_trait::async_trait]
impl<'r, 'c> WalletsDal for VerifierWalletsConn<'r, 'c> {
    async fn load_system_wallets(&mut self) -> anyhow::Result<zksync_types::via_wallet::SystemWallets> {
        let map = match self.inner.via_wallet_dal().get_system_wallets_raw().await? {
            Some(map) => map,
            None => Default::default(),
        };
        Ok(zksync_types::via_wallet::SystemWallets::try_from(map)?)
    }

    async fn insert_wallets(&mut self, details: &zksync_types::via_wallet::SystemWalletsDetails) -> anyhow::Result<()> {
        Ok(self.inner.via_wallet_dal().insert_wallets(details).await?)
    }
}

#[async_trait::async_trait]
impl MessageProcessor for SystemWalletProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        indexer: &mut BitcoinInscriptionIndexer,
    ) -> Result<bool, MessageProcessorError> {
        let mut adapter = VerifierWalletsConn { inner: storage };
        via_btc_watch_common::system_wallet::SystemWalletProcessorApi::process_messages(
            self,
            &mut adapter,
            msgs,
            indexer,
        )
        .await
        .map_err(|e| MessageProcessorError::Internal(e.into()))
    }
}
