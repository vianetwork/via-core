use std::sync::Arc;
use anyhow::Context;
use via_btc_client::traits::BitcoinOps;
use via_verifier_dal::Verifier;
use zksync_dal::ConnectionPool;

#[derive(Debug, Clone)]
pub struct ViaState {
    pool: ConnectionPool<Verifier>,
    btc_client: Arc<dyn BitcoinOps>,
}
impl ViaState {
    pub fn new(pool: ConnectionPool<Verifier>, btc_client: Arc<dyn BitcoinOps>) -> Self {
        Self { pool, btc_client }
    }
    pub async fn is_sync_in_progress(&self) -> anyhow::Result<bool> {
        let mut storage = self.pool.connection().await?;
        let mut reader = via_btc_storage::ReaderDal::new(&mut storage);
        let state = reader.state().await?.context("Bitcoin reader is not initialized")?;
        let selected = self.btc_client.best_block_hash().await?;
        let tip = self
            .btc_client
            .header_by_hash(&selected)
            .await?
            .context("selected Bitcoin header unavailable")?;
        let mature = tip.height.saturating_sub(self.btc_client.reorg_safe_depth()?.get() - 1);
        let consumer = reader
            .consumer(via_btc_storage::PROTOCOL_CONSUMER)
            .await?
            .context("protocol consumer is missing")?;
        Ok(!state.replay_ready || consumer.applied.header.height < mature)
    }
    pub async fn is_reorg_in_progress(&self) -> anyhow::Result<bool> {
        let mut storage = self.pool.connection().await?;
        Ok(via_btc_storage::ReaderDal::new(&mut storage)
            .state()
            .await?
            .context("Bitcoin reader is not initialized")?
            .stopped_reason
            .is_some())
    }
}
