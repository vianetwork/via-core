use via_btc_client::bootstrap::ViaBootstrap;
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};

#[derive(Debug, Clone)]
pub struct ViaWalletsInitializer {
    pool: ConnectionPool<Verifier>,
    bootstrap: ViaBootstrap,
}

impl ViaWalletsInitializer {
    pub fn new(pool: ConnectionPool<Verifier>, bootstrap: ViaBootstrap) -> Self {
        Self { pool, bootstrap }
    }

    pub async fn load_system_wallets(
        pool: ConnectionPool<Verifier>,
    ) -> anyhow::Result<SystemWallets> {
        if let Some(system_wallet) =
            ViaWalletsInitializer::fetch_indexer_wallets_from_db(&pool).await?
        {
            return Ok(system_wallet);
        }
        anyhow::bail!("System wallets not initialized")
    }

    pub async fn fetch_indexer_wallets_from_db(
        pool: &ConnectionPool<Verifier>,
    ) -> anyhow::Result<Option<SystemWallets>> {
        let system_wallet_raw_opt = pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw()
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
    pub async fn is_initialized(&self) -> anyhow::Result<bool> {
        Ok(
            ViaWalletsInitializer::fetch_indexer_wallets_from_db(&self.pool)
                .await?
                .is_some(),
        )
    }

    pub async fn initialize_storage(&self) -> anyhow::Result<()> {
        if !self.is_initialized().await? {
            let state = self.bootstrap.process_bootstrap_messages().await?;

            let indexer_wallets_details = SystemWalletsDetails::try_from(&state)?;

            self.pool
                .connection()
                .await?
                .via_wallet_dal()
                .insert_wallets(&indexer_wallets_details, state.starting_block_number as i64)
                .await?;

            tracing::info!("System wallets initialized");
        }
        Ok(())
    }
}
