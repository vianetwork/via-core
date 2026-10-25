use via_verifier_dal::{ConnectionPool, Verifier};
use zksync_types::via_wallet::SystemWallets;

pub struct ViaWalletsInitializer;

impl ViaWalletsInitializer {
    pub async fn load_system_wallets(
        pool: ConnectionPool<Verifier>,
    ) -> anyhow::Result<SystemWallets> {
        let mut connection = pool.connection().await?;
        via_btc_storage::ReaderDal::new(&mut connection).current_wallets().await
    }
}
