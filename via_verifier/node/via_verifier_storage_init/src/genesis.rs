use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal as _};
use zksync_types::{via_bootstrap::BootstrapState, H256};

#[derive(Debug)]
pub struct VerifierGenesis {
    pub bootstrap: BootstrapState,
    pub pool: ConnectionPool<Verifier>,
}

impl VerifierGenesis {
    pub async fn initialize_storage(&self) -> anyhow::Result<()> {
        if self.is_initialized().await? {
            return Ok(());
        }

        let mut storage = self.pool.connection_tagged("verifier_genesis").await?;
        let mut transaction = storage.start_transaction().await?;

        transaction
            .via_protocol_versions_dal()
            .save_protocol_version(
                self.bootstrap.protocol_version,
                self.bootstrap.bootloader_hash.as_bytes(),
                self.bootstrap.abstract_account_hash.as_bytes(),
                H256::zero().as_bytes(),
                self.bootstrap.snark_wrapper_vk_hash.as_bytes(),
            )
            .await?;

        transaction
            .via_protocol_versions_dal()
            .mark_upgrade_as_executed(H256::zero().as_bytes())
            .await?;

        transaction.commit().await?;

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
