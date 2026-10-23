use via_btc_client::bootstrap::ViaBootstrap;
use zksync_dal::{Connection, ConnectionPool, Core, CoreDal};
use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};

#[derive(Debug, Clone)]
pub struct ViaWalletsInitializer {
    pool: ConnectionPool<Core>,
    bootstrap: ViaBootstrap,
}

impl ViaWalletsInitializer {
    pub fn new(pool: ConnectionPool<Core>, bootstrap: ViaBootstrap) -> Self {
        Self { pool, bootstrap }
    }

    pub async fn load_system_wallets(pool: ConnectionPool<Core>) -> anyhow::Result<SystemWallets> {
        if let Some(system_wallet) =
            ViaWalletsInitializer::fetch_indexer_wallets_from_db(&pool).await?
        {
            return Ok(system_wallet);
        }
        anyhow::bail!("System wallets not initialized")
    }

    pub async fn fetch_indexer_wallets_from_db(
        pool: &ConnectionPool<Core>,
    ) -> anyhow::Result<Option<SystemWallets>> {
        let mut storage = pool.connection().await?;
        Self::fetch_indexer_wallets(&mut storage).await
    }

    async fn fetch_indexer_wallets(
        storage: &mut Connection<'_, Core>,
    ) -> anyhow::Result<Option<SystemWallets>> {
        let last_processed_l1_block = storage
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await?;

        let system_wallet_raw_opt = storage
            .via_wallet_dal()
            .get_system_wallets_raw(last_processed_l1_block as i64)
            .await?;
        let Some(system_wallet_raw) = system_wallet_raw_opt else {
            return Ok(None);
        };

        let parsed_system_wallets = SystemWallets::try_from(system_wallet_raw)?;
        tracing::info!("Loaded the indexer wallets from DB");

        Ok(Some(parsed_system_wallets))
    }
}

impl ViaWalletsInitializer {
    pub async fn initialize_storage(&self) -> anyhow::Result<()> {
        let mut storage = self.pool.connection().await?;
        let mut transaction = storage.start_transaction().await?;
        transaction.via_indexer_dal().lock_btc_watch().await?;
        if Self::fetch_indexer_wallets(&mut transaction)
            .await?
            .is_none()
        {
            let state = self.bootstrap.process_bootstrap_messages().await?;

            let indexer_wallets_details = SystemWalletsDetails::try_from(&state)?;

            transaction
                .via_wallet_dal()
                .insert_bootstrap_wallets(&indexer_wallets_details, state.starting_block_number as i64)
                .await?;

            transaction.protocol_versions_dal().mark_via_bootstrap_version(state.protocol_version,state.base_system_contracts_hashes(),zksync_types::protocol_version::L1VerifierConfig { snark_wrapper_vk_hash:state.snark_wrapper_vk_hash }).await?;

            tracing::info!("System wallets initialized");
        }
        transaction.commit().await?;
        Ok(())
    }
}
