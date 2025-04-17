use tokio::sync::watch;
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal as _};
use zksync_config::GenesisConfig;
use zksync_node_storage_init::InitializeStorage;
use zksync_types::H256;

#[derive(Debug)]
pub struct VerifierGenesis {
    pub genesis_config: GenesisConfig,
    pub pool: ConnectionPool<Verifier>,
}

#[async_trait::async_trait]
impl InitializeStorage for VerifierGenesis {
    async fn initialize_storage(
        &self,
        _stop_receiver: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut storage = self.pool.connection_tagged("verifier_genesis").await?;

        if self.is_initialized().await? {
            return Ok(());
        }

        storage
            .via_protocol_versions_dal()
            .save_protocol_version(
                self.genesis_config
                    .protocol_version
                    .ok_or_else(|| anyhow::anyhow!("protocol_version is not set"))?,
                self.genesis_config
                    .bootloader_hash
                    .ok_or_else(|| anyhow::anyhow!("bootloader_hash is not set"))?
                    .as_bytes(),
                self.genesis_config
                    .default_aa_hash
                    .ok_or_else(|| anyhow::anyhow!("default_aa_hash is not set"))?
                    .as_bytes(),
                H256::zero().as_bytes(),
                self.genesis_config
                    .recursion_scheduler_level_vk_hash
                    .as_bytes(),
            )
            .await?;

        storage
            .via_protocol_versions_dal()
            .mark_upgrade_as_executed(H256::zero().as_bytes())
            .await?;
        Ok(())
    }

    async fn is_initialized(&self) -> anyhow::Result<bool> {
        let mut storage = self.pool.connection_tagged("verifier_genesis").await?;

        Ok(storage
            .via_protocol_versions_dal()
            .latest_protocol_semantic_version()
            .await?
            .is_some())
    }
}
