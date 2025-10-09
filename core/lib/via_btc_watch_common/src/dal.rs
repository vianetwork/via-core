use async_trait::async_trait;
use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};

#[async_trait]
pub trait IndexerMetaDal {
    async fn get_last_processed_l1_block(&mut self, module: &str) -> anyhow::Result<u32>;
    async fn init_indexer_metadata(&mut self, module: &str, start_block: u32)
        -> anyhow::Result<()>;
    async fn update_last_processed_l1_block(
        &mut self,
        module: &str,
        new_block: u32,
    ) -> anyhow::Result<()>;
}

#[async_trait]
pub trait WalletsDal {
    async fn load_system_wallets(&mut self) -> anyhow::Result<SystemWallets>;
    async fn insert_wallets(&mut self, details: &SystemWalletsDetails) -> anyhow::Result<()>;
}
