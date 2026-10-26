mod metrics;
mod withdrawal;

use std::sync::Arc;
use tokio::sync::watch;
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, WithdrawalScanMode},
};
use via_btc_storage::ReaderDal;
use via_musig2::{transaction_builder::TransactionBuilder, types::TransactionBuilderConfig};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use zksync_config::ViaBtcWatchConfig;
use crate::metrics::METRICS;

#[derive(Debug)]
pub struct VerifierBtcWatch {
    config: ViaBtcWatchConfig,
    client: Arc<BitcoinClient>,
    indexer: BitcoinInscriptionIndexer,
    pool: ConnectionPool<Verifier>,
    withdrawal_builder: TransactionBuilder,
    withdrawal_fulfillment: Option<WithdrawalFulfillment>,
}
struct WithdrawalFulfillment {
    config: TransactionBuilderConfig,
}
impl std::fmt::Debug for WithdrawalFulfillment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WithdrawalFulfillment").finish_non_exhaustive()
    }
}
impl VerifierBtcWatch {
    pub async fn new(
        config: ViaBtcWatchConfig,
        indexer: BitcoinInscriptionIndexer,
        client: Arc<BitcoinClient>,
        pool: ConnectionPool<Verifier>,
    ) -> anyhow::Result<Self> {
        let withdrawal_builder = TransactionBuilder::new(client.clone())?;
        Ok(Self { config, client, indexer, pool, withdrawal_builder, withdrawal_fulfillment: None })
    }
    pub fn with_withdrawal_fulfillment(
        mut self,
        config: TransactionBuilderConfig,
        confirmations: u32,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(confirmations > 0, "Withdrawal fulfillment requires positive confirmations");
        self.withdrawal_fulfillment = Some(WithdrawalFulfillment { config });
        Ok(self)
    }
    pub async fn run(mut self, mut stop: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut observer = withdrawal::WithdrawalObserver::new(
            self.client.clone(),
            self.indexer.clone(),
            self.pool.clone(),
        );
        let observer_stop = stop.clone();
        let interval = self.config.poll_interval();
        self.indexer = self.indexer.with_withdrawal_mode(WithdrawalScanMode::Ignore);
        let protocol = async {
            let mut timer = tokio::time::interval(interval);
            let mut progressed = false;
            while !*stop.borrow_and_update() {
                if !progressed {
                    tokio::select! { _ = timer.tick() => {}, _ = stop.changed() => break }
                }
                let result = async {
                    let mut storage = self.pool.connection_tagged("bitcoin_reader").await?;
                    via_btc_storage::transition::process_next(
                        &mut storage,
                        &self.client,
                        &mut self.indexer,
                        via_consensus::consensus::BATCH_FINALIZATION_THRESHOLD,
                    )
                    .await
                }
                .await;
                match result {
                    Ok(next) => progressed = next,
                    Err(error) => {
                        progressed = false;
                        if !via_btc_storage::transition::is_held(&error) {
                            return Err(error);
                        }
                        METRICS.errors.inc();
                        tracing::warn!(%error, "Bitcoin reader held");
                    }
                }
                if !progressed {
                    if let Err(error) = self.reconcile_withdrawals().await {
                        tracing::debug!(%error, "Withdrawal fulfillment held");
                    }
                }
            }
            Ok(())
        };
        tokio::try_join!(protocol, observer.run(observer_stop, interval))?;
        Ok(())
    }
    async fn reconcile_withdrawals(&self) -> anyhow::Result<()> {
        let Some(policy) = &self.withdrawal_fulfillment else { return Ok(()) };
        {
            let mut storage = self.pool.connection().await?;
            if ReaderDal::new(&mut storage).mature_wallets().await?.bridge
                != policy.config.bridge_address
            {
                return Ok(());
            }
        }
        let mut tx =
            via_btc_storage::transition::begin_authority(&self.pool, self.client.as_ref(), &[])
                .await?;
        anyhow::ensure!(
            ReaderDal::new(&mut tx).mature_wallets().await?.bridge == policy.config.bridge_address,
            via_btc_storage::StoreError::AuthorityHeld
        );
        // Authority holds frontier at selected tip minus N plus one, which gives back checked tip here.
        let tip = ReaderDal::new(&mut tx).state().await?.unwrap().confirmed.height
            + self.client.reorg_safe_depth()?.get()
            - 1;
        let wallet = policy.config.bridge_address.script_pubkey();
        let observations =
            tx.via_withdrawal_dal().pending_withdrawal_observations(wallet.as_bytes()).await?;
        for observation in observations {
            let references = observation.withdrawals.iter().map(|output| output.reference.clone()).collect::<Vec<_>>();
            let requests =
                match tx.via_withdrawal_dal().load_expected_withdrawals(wallet.as_bytes(), &references).await {
                    Ok(requests) => requests,
                    Err(err) => {
                        tracing::debug!("Withdrawal expected evidence not ready: {err:#}");
                        continue;
                    }
                };
            if let Err(err) =
                self.withdrawal_builder.verify_observed_withdrawal(&observation, &requests, &policy.config)
            {
                tracing::warn!("Withdrawal observation remains held: {err:#}");
                continue;
            }
            if tx
                .via_withdrawal_dal()
                .fulfill_withdrawal_observation(
                    wallet.as_bytes(),
                    &observation,
                    &requests,
                    tip,
                    self.client.reorg_safe_depth()?.get(),
                )
                .await?
            {
                METRICS.withdrawal_confirmed.inc();
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
