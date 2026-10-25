use std::{sync::Arc, time::Duration};

use anyhow::Context;
use tokio::sync::watch;
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, WithdrawalScanMode},
    traits::BitcoinOps,
    types::FullInscriptionMessage,
};
use via_btc_storage::{
    transition::{PreparedTransition, ReaderSnapshot},
    ReaderDal, ReaderRole,
};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::withdrawal_observation::{
    ObservedWithdrawal, WithdrawalInclusion, WithdrawalObservation,
};

pub(super) struct WithdrawalObserver {
    client: Arc<BitcoinClient>,
    indexer: BitcoinInscriptionIndexer,
    pool: ConnectionPool<Verifier>,
}
impl WithdrawalObserver {
    pub fn new(
        client: Arc<BitcoinClient>,
        indexer: BitcoinInscriptionIndexer,
        pool: ConnectionPool<Verifier>,
    ) -> Self {
        Self { client, indexer: indexer.with_withdrawal_mode(WithdrawalScanMode::Required), pool }
    }
    pub async fn run(
        &mut self,
        mut stop: watch::Receiver<bool>,
        interval: Duration,
    ) -> anyhow::Result<()> {
        while !*stop.borrow_and_update() {
            let result = tokio::select! { result = self.process_next() => result, _ = stop.changed() => break };
            let progressed = match result {
                Ok(progressed) => progressed,
                Err(error) => {
                    if !via_btc_storage::transition::is_held(&error) {
                        return Err(error);
                    }
                    tracing::warn!(%error, "Withdrawal observer held");
                    false
                }
            };
            if !progressed {
                tokio::select! { _ = tokio::time::sleep(interval) => {}, _ = stop.changed() => break }
            }
        }
        Ok(())
    }
    async fn process_next(&mut self) -> anyhow::Result<bool> {
        let mut storage = self.pool.connection_tagged("withdrawal_observer").await?;
        let snapshot = ReaderSnapshot::load(
            &mut ReaderDal::new(&mut storage),
            self.client.reorg_safe_depth()?,
        )
        .await?;
        let prepared = PreparedTransition::prepare(self.client.as_ref(), snapshot).await?;
        if !matches!(
            prepared.transition,
            via_btc_storage::transition::Transition::Idle
                | via_btc_storage::transition::Transition::Append { .. }
        ) {
            return Ok(false);
        }
        let position = ReaderDal::new(&mut storage)
            .consumer("withdrawals")
            .await?
            .context("withdrawal consumer missing")?;
        let next =
            position.applied.header.height.checked_add(1).context("Bitcoin height overflow")?;
        let Some(header) = ReaderDal::new(&mut storage).header_at(next).await? else {
            return Ok(false);
        };
        let mut wallets = ReaderDal::new(&mut storage).mature_wallets().await?;
        if let Some(script) = storage.via_withdrawal_dal().active_wallet().await? {
            wallets.bridge = bitcoin::Address::from_script(&script, self.client.get_network())?;
        }
        self.indexer.update_system_wallets(
            Some(wallets.sequencer),
            Some(wallets.bridge),
            Some(wallets.verifiers),
            Some(wallets.governance),
        );
        let source = self.client.canonical_header_at(next).await?;
        anyhow::ensure!(
            source.hash == header.header.block_hash,
            via_btc_storage::StoreError::SourceHeld("withdrawal header changed")
        );
        let block = self.client.checked_block(&source).await?;
        let msgs =
            self.indexer.process_block_body(&block, next).await.context(
                via_btc_storage::StoreError::SourceHeld("withdrawal inputs unavailable"),
            )?;
        prepared.revalidate(self.client.as_ref()).await?;
        let mut tx = <Verifier as ReaderRole>::OWNER.begin(&mut storage).await?;
        <Verifier as ReaderRole>::lock_source(&mut tx).await?;
        anyhow::ensure!(
            ReaderDal::new(&mut tx).get_highest_block().await?.as_ref()
                == Some(&prepared.snapshot.head),
            via_btc_storage::StoreError::HeadChanged
        );
        for msg in msgs {
            if let FullInscriptionMessage::BridgeWithdrawal(message) = msg {
                let wallet = message
                    .input
                    .bridge_script_pubkey
                    .ok_or_else(|| anyhow::anyhow!("Missing verified withdrawal wallet context"))?;
                let block_hash = message
                    .input
                    .block_hash
                    .ok_or_else(|| anyhow::anyhow!("Missing withdrawal block inclusion"))?;
                let observation = WithdrawalObservation {
                    transaction: message.input.transaction,
                    prevouts: message.input.prevouts,
                    withdrawals: message
                        .input
                        .withdrawals
                        .into_iter()
                        .map(ObservedWithdrawal::from)
                        .collect(),
                    inclusion: Some(WithdrawalInclusion {
                        block_hash,
                        block_height: message.common.block_height,
                    }),
                };
                tx.via_withdrawal_dal()
                    .record_withdrawal_observation(wallet.as_bytes(), &observation)
                    .await?;
            }
        }
        ReaderDal::new(&mut tx)
            .complete_consumer("withdrawals", &header, &position.oldest_needed)
            .await?;
        tx.commit().await?;
        via_btc_storage::metrics::METRICS.consumer
            [&(<Verifier as ReaderRole>::NAME, "withdrawals")]
            .set(next as usize);
        Ok(true)
    }
}
