use std::time::Instant;

use futures::{stream, TryStreamExt};

use super::{Aborted, AccountLifespan};
use crate::{
    command::SubscriptionType,
    config::RequestLimiters,
    report::{ReportBuilder, ReportLabel},
    rng::WeightedRandom,
};

impl AccountLifespan {
    async fn run_single_subscription(mut self, limiters: &RequestLimiters) -> Result<(), Aborted> {
        let permit = limiters.subscriptions.acquire().await.unwrap();
        let subscription_type = SubscriptionType::random(&mut self.eth_wallet.rng);
        let start = Instant::now();

        // Skip the action if the contract is not yet initialized for the account.
        let label = if let (SubscriptionType::Logs, None) = (
            subscription_type,
            self.eth_wallet.deployed_contract_address.get(),
        ) {
            ReportLabel::skipped("Contract not deployed yet")
        } else {
            ReportLabel::ActionDone
        };
        drop(permit);

        let report = ReportBuilder::default()
            .action(subscription_type)
            .label(label)
            .time(start.elapsed())
            .reporter(self.eth_wallet.wallet.address())
            .finish();
        self.send_report(report).await?;
        Ok(())
    }

    pub(super) async fn run_pubsub_task(self, limiters: &RequestLimiters) -> Result<(), Aborted> {
        // We use `try_for_each_concurrent` to propagate test abortion, but we cannot
        // rely solely on its concurrency limiter because we need to limit concurrency
        // for all accounts in total, rather than for each account separately.
        let local_limit = (self.config.sync_pubsub_subscriptions_limit / 5).max(1);
        let subscriptions = stream::repeat_with(move || Ok(self.clone()));
        subscriptions
            .try_for_each_concurrent(local_limit, |this| this.run_single_subscription(limiters))
            .await
    }
}
