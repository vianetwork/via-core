use std::sync::Arc;

use anyhow::anyhow;
use futures::{channel::mpsc, future, SinkExt};
use via_btc_client::{
    client::BitcoinClient,
    traits::BitcoinOps,
    types::{BitcoinNetwork, NodeAuth},
};
use zksync_types::{L2_BASE_TOKEN_ADDRESS, U256};

use crate::{
    account::{btc_deposit, AccountLifespan},
    account_pool::AccountPool,
    config::{ExecutionConfig, LoadtestConfig, RequestLimiters},
    constants::*,
    metrics::LOADTEST_METRICS,
    report::ReportBuilder,
    report_collector::{LoadtestResult, ReportCollector},
    sdk::ZksNamespaceClient,
};

/// Executor is the entity capable of running the loadtest flow.
///
/// It takes care of the following topics:
///
/// - Minting the tokens on L1 for the main account.
/// - Depositing tokens to the main account in L2 and unlocking it.
/// - Spawning the report collector.
/// - Distributing the funds among the test wallets.
/// - Spawning account lifespan futures.
/// - Awaiting for all the account futures to complete.
/// - Getting the final test resolution from the report collector.
pub struct Executor {
    config: LoadtestConfig,
    execution_config: ExecutionConfig,
    pool: AccountPool,
}

impl Executor {
    /// Creates a new Executor entity.
    pub async fn new(
        config: LoadtestConfig,
        execution_config: ExecutionConfig,
    ) -> anyhow::Result<Self> {
        let pool = AccountPool::new(&config).await?;

        Ok(Self {
            config,
            execution_config,
            pool,
        })
    }

    /// Runs the loadtest until the completion.
    pub async fn start(&mut self) -> LoadtestResult {
        // If the error occurs during the main flow, we will consider it as a test failure.
        self.start_inner().await.unwrap_or_else(|err| {
            tracing::error!("Loadtest was interrupted by the following error: {err}");
            LoadtestResult::TestFailed
        })
    }

    /// Inner representation of `start` function which returns a `Result`, so it can conveniently use `?`.
    async fn start_inner(&mut self) -> anyhow::Result<LoadtestResult> {
        tracing::info!("Initializing accounts");
        tracing::info!(
            "Running for MASTER {:?}",
            self.pool.eth_master_wallet.address()
        );
        self.check_btc_balance().await?;

        // Deposit BTC to the master account
        self.deposit_btc_to_master().await?;

        // Deposit BTC to the paymaster
        self.deposit_btc_to_paymaster().await?;

        // Distribute BTC on L1 to the accounts & Distribute BTC on L2 to the accounts
        self.distribute_btc(self.config.accounts_amount).await?;

        let final_result = self.initial_tests().await?;
        Ok(final_result)
    }

    /// Checks if the master account has enough BTC balance to run the loadtest
    async fn check_btc_balance(&mut self) -> anyhow::Result<()> {
        tracing::info!("Master Account: Checking BTC balance");
        let master_wallet = &mut self.pool.btc_master_wallet;

        let btc_client = BitcoinClient::new(
            &self.config.l1_btc_rpc_address,
            BitcoinNetwork::Regtest,
            NodeAuth::UserPass(
                self.config.l1_btc_rpc_username.clone(),
                self.config.l1_btc_rpc_password.clone(),
            ),
        )?;

        let btc_balance = btc_client.get_balance(&master_wallet.btc_address).await?;
        if btc_balance < bitcoin::Amount::from_btc(600.0).unwrap().to_sat().into() {
            anyhow::bail!(
                "BTC balance on {} is too low to safely perform the loadtest: {} - at least 600 BTC is required",
                master_wallet.btc_address,
                btc_balance
            );
        }
        tracing::info!(
            "Master Account {} L1 BTC balance is {} sats",
            master_wallet.btc_address,
            btc_balance
        );

        LOADTEST_METRICS
            .master_account_balance
            .set(btc_balance as f64);

        Ok(())
    }

    /// Deposits BTC to the master account
    async fn deposit_btc_to_master(&mut self) -> anyhow::Result<()> {
        tracing::info!("Master Account: Depositing BTC");
        let master_wallet = &mut self.pool.btc_master_wallet;

        let deposit_amount = bitcoin::Amount::from_btc(300.0).unwrap().to_sat();

        let deposit_response = btc_deposit::deposit(
            deposit_amount,
            self.pool.eth_master_wallet.address(),
            master_wallet.btc_private_key,
            self.config.l1_btc_rpc_address.clone(),
            self.config.l1_btc_rpc_username.clone(),
            self.config.l1_btc_rpc_password.clone(),
        )
        .await;

        match deposit_response {
            Ok(hash) => {
                tracing::info!("BTC deposit transaction sent with hash: {}", hash);
                Ok(())
            }
            Err(err) => {
                anyhow::bail!("Failed to deposit BTC to master account: {}", err);
            }
        }
    }

    /// Deposits BTC to the paymaster
    async fn deposit_btc_to_paymaster(&mut self) -> anyhow::Result<()> {
        tracing::info!("Master Account: Depositing BTC to paymaster");
        let master_wallet = &mut self.pool.btc_master_wallet;

        let paymaster_address = self
            .pool
            .eth_master_wallet
            .provider
            .get_testnet_paymaster()
            .await?
            .expect("No testnet paymaster is set");

        let deposit_amount = bitcoin::Amount::from_btc(50.0).unwrap().to_sat();

        let deposit_response = btc_deposit::deposit(
            deposit_amount,
            paymaster_address,
            master_wallet.btc_private_key,
            self.config.l1_btc_rpc_address.clone(),
            self.config.l1_btc_rpc_username.clone(),
            self.config.l1_btc_rpc_password.clone(),
        )
        .await;

        match deposit_response {
            Ok(hash) => {
                tracing::info!("BTC deposit to paymaster sent with hash: {}", hash);
                Ok(())
            }
            Err(err) => {
                anyhow::bail!("Failed to deposit BTC to paymaster: {}", err);
            }
        }
    }

    /// Distributes BTC to test accounts on L2
    async fn distribute_btc(&mut self, accounts_to_process: usize) -> anyhow::Result<()> {
        tracing::info!("Master Account: Distributing BTC to test accounts on L2");
        let master_eth_wallet = &mut self.pool.eth_master_wallet;

        let l2_transfer_amount = bitcoin::Amount::from_btc(0.01).unwrap().to_sat();

        for eth_account in self.pool.eth_accounts.iter().take(accounts_to_process) {
            // L2 BTC transfer
            let transfer_builder = master_eth_wallet
                .start_transfer()
                .to(eth_account.wallet.address())
                .token(L2_BASE_TOKEN_ADDRESS)
                .amount(U256::from(l2_transfer_amount));

            // Estimate fee first
            let fee = transfer_builder
                .estimate_fee(None)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to estimate fee: {}", e))?;

            let transfer = transfer_builder.fee(fee).send().await;

            match transfer {
                Ok(handle) => {
                    let receipt = handle.wait_for_commit().await;
                    match receipt {
                        Ok(_) => {
                            tracing::info!(
                                "BTC L2 transfer sent to account {}",
                                eth_account.wallet.address()
                            );
                        }
                        Err(err) => {
                            anyhow::bail!("Failed to get receipt for BTC L2 transfer: {}", err);
                        }
                    }
                }
                Err(err) => {
                    anyhow::bail!("Failed to transfer BTC on L2: {}", err);
                }
            }
        }

        Ok(())
    }

    /// Initializes the loadtest by doing the following:
    ///
    /// - Spawning the `ReportCollector`.
    /// - Distributing ERC-20 token in L2 among test wallets via `Transfer` operation.
    /// - Distributing ETH in L1 among test wallets in order to make them able to perform priority operations.
    /// - Spawning test account routine futures.
    /// - Completing all the spawned tasks and returning the result to the caller.
    async fn initial_tests(&mut self) -> anyhow::Result<LoadtestResult> {
        tracing::info!("Master Account: Sending initial transfers");

        // Prepare channels for the report collector.
        let (mut report_sender, report_receiver) = mpsc::channel(256);

        let report_collector = ReportCollector::new(
            report_receiver,
            self.config.expected_tx_count,
            self.config.duration(),
            self.config.prometheus_label.clone(),
            self.config.fail_fast,
        );
        let report_collector_future = tokio::spawn(report_collector.run());

        let config = &self.config;
        let accounts_amount = config.accounts_amount;
        let addresses = self.pool.addresses.clone();
        let paymaster_address = self
            .pool
            .eth_master_wallet
            .provider
            .get_testnet_paymaster()
            .await?
            .expect("No testnet paymaster is set");

        let mut accounts_processed = 0;
        let limiters = Arc::new(RequestLimiters::new(config));

        let mut account_tasks = vec![];
        while accounts_processed != accounts_amount {
            let accounts_left = accounts_amount - accounts_processed;
            let max_accounts_per_iter = MAX_OUTSTANDING_NONCE;
            let accounts_to_process = std::cmp::min(accounts_left, max_accounts_per_iter);

            accounts_processed += accounts_to_process;
            tracing::info!("[{accounts_processed}/{accounts_amount}] Accounts processed");

            let contract_execution_params = self.execution_config.contract_execution_params.clone();
            // Spawn each account lifespan.

            anyhow::ensure!(
                !report_sender.is_closed(),
                "test aborted; see reporter logs for details"
            );

            let new_account_futures = self
                .pool
                .eth_accounts
                .drain(..accounts_to_process)
                .zip(self.pool.btc_accounts.drain(..accounts_to_process))
                .map(|(eth_wallet, btc_wallet)| {
                    let account = AccountLifespan::new(
                        config,
                        contract_execution_params.clone(),
                        addresses.clone(),
                        eth_wallet,
                        btc_wallet,
                        report_sender.clone(),
                        paymaster_address,
                    );
                    let limiters = Arc::clone(&limiters);
                    tokio::spawn(async move { account.run(&limiters).await })
                });
            account_tasks.extend(new_account_futures);
        }

        report_sender
            .send(ReportBuilder::build_init_complete_report())
            .await
            .map_err(|_| anyhow!("test aborted; see reporter logs for details"))?;
        drop(report_sender);
        // ^ to terminate `report_collector_future` once all `account_futures` are finished

        assert!(
            self.pool.eth_accounts.is_empty(),
            "Some accounts were not drained"
        );
        tracing::info!("All the initial transfers are completed");

        tracing::info!("Waiting for the account futures to be completed...");
        future::try_join_all(account_tasks).await?;
        tracing::info!("All the spawned tasks are completed");

        Ok(report_collector_future.await?)
    }
}
