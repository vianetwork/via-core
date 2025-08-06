mod genesis;
mod wallets;

use std::{future::Future, sync::Arc, time::Duration};

use genesis::VerifierGenesis;
use tokio::sync::watch;
use via_btc_client::client::BitcoinClient;
use via_verifier_dal::{ConnectionPool, Verifier};
use wallets::ViaWalletsInitializer;
use zksync_config::{configs::via_consensus::ViaGenesisConfig, GenesisConfig};
use zksync_node_storage_init::InitializeStorage;
use zksync_types::via_wallet::SystemWallets;

#[derive(Debug, Clone)]
pub struct ViaVerifierStorageInitializer {
    genesis: Arc<dyn InitializeStorage>,
    wallets: ViaWalletsInitializer,
}

impl ViaVerifierStorageInitializer {
    pub fn new(
        via_genesis_config: ViaGenesisConfig,
        genesis_config: GenesisConfig,
        pool: ConnectionPool<Verifier>,
        client: Arc<BitcoinClient>,
    ) -> Self {
        let genesis = Arc::new(VerifierGenesis {
            genesis_config,
            pool: pool.clone(),
        });
        let wallets = ViaWalletsInitializer::new(pool, client, via_genesis_config);
        Self { genesis, wallets }
    }

    pub async fn indexer_wallets(&self) -> anyhow::Result<SystemWallets> {
        if let Some(system_wallets) = self.wallets.fetch_indexer_wallets_from_db().await? {
            return Ok(system_wallets);
        }
        Ok(self.wallets.init_indexer_wallets().await?)
    }

    pub async fn run(self, stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        self.genesis
            .initialize_storage(stop_receiver.clone())
            .await?;

        Ok(())
    }

    /// Checks if the node can safely start operating.
    pub async fn wait_for_initialized_storage(
        &self,
        stop_receiver: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        const POLLING_INTERVAL: Duration = Duration::from_secs(1);

        // Wait until data is added to the database.
        poll(stop_receiver.clone(), POLLING_INTERVAL, || {
            self.is_database_initialized()
        })
        .await?;
        if *stop_receiver.borrow() {
            return Ok(());
        }

        Ok(())
    }

    async fn is_database_initialized(&self) -> anyhow::Result<bool> {
        // We're fine if the database is initialized in any meaningful way we can check.
        if self.genesis.is_initialized().await? {
            return Ok(true);
        }
        Ok(false)
    }
}

async fn poll<F, Fut>(
    mut stop_receiver: watch::Receiver<bool>,
    polling_interval: Duration,
    mut check: F,
) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<bool>>,
{
    while !*stop_receiver.borrow() && !check().await? {
        // Return value will be checked on the next iteration.
        tokio::time::timeout(polling_interval, stop_receiver.changed())
            .await
            .ok();
    }

    Ok(())
}
