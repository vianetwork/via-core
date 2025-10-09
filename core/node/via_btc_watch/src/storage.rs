use via_btc_watch_common::dal::{IndexerMetaDal, WalletsDal};
use zksync_dal::{ConnectionPool, Core, CoreDal};

#[derive(Clone)]
pub struct SequencerStorage {
    pub pool: ConnectionPool<Core>,
}

#[async_trait::async_trait]
impl IndexerMetaDal for SequencerStorage {
    async fn get_last_processed_l1_block(&mut self, module: &str) -> anyhow::Result<u32> {
        let mut conn = self.pool.connection_tagged("via_btc_watch").await?;
        Ok(conn
            .via_indexer_dal()
            .get_last_processed_l1_block(module)
            .await? as u32)
    }

    async fn init_indexer_metadata(
        &mut self,
        module: &str,
        start_block: u32,
    ) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("via_btc_watch").await?;
        Ok(conn
            .via_indexer_dal()
            .init_indexer_metadata(module, start_block)
            .await?)
    }

    async fn update_last_processed_l1_block(
        &mut self,
        module: &str,
        new_block: u32,
    ) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("via_btc_watch").await?;
        Ok(conn
            .via_indexer_dal()
            .update_last_processed_l1_block(module, new_block)
            .await?)
    }
}

#[async_trait::async_trait]
impl WalletsDal for SequencerStorage {
    async fn load_system_wallets(
        &mut self,
    ) -> anyhow::Result<zksync_types::via_wallet::SystemWallets> {
        let mut conn = self.pool.connection_tagged("via_btc_watch").await?;
        let map = match conn.via_wallet_dal().get_system_wallets_raw().await? {
            Some(map) => map,
            None => Default::default(),
        };
        Ok(zksync_types::via_wallet::SystemWallets::try_from(map)?)
    }

    async fn insert_wallets(
        &mut self,
        details: &zksync_types::via_wallet::SystemWalletsDetails,
    ) -> anyhow::Result<()> {
        let mut conn = self.pool.connection_tagged("via_btc_watch").await?;
        Ok(conn.via_wallet_dal().insert_wallets(details).await?)
    }
}
