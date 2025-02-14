use std::sync::Arc;

use anyhow::anyhow;
use futures::{channel::mpsc, future, SinkExt};
use via_btc_client::{
    client::BitcoinClient,
    traits::BitcoinOps,
    types::{BitcoinNetwork, NodeAuth},
};
use zksync_eth_client::Options;
use zksync_eth_signer::PrivateKeySigner;
use zksync_system_constants::MAX_L1_TRANSACTION_GAS_LIMIT;
use zksync_types::{
    api::BlockNumber, tokens::ETHEREUM_ADDRESS, Address, Nonce,
    REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE, U256, U64,
};

use crate::{
    account::{btc_deposit, AccountLifespan},
    account_pool::AccountPool,
    config::{ExecutionConfig, LoadtestConfig, RequestLimiters},
    constants::*,
    metrics::LOADTEST_METRICS,
    report::ReportBuilder,
    report_collector::{LoadtestResult, ReportCollector},
    sdk::{
        ethereum::{PriorityOpHolder, DEFAULT_PRIORITY_FEE},
        utils::{
            get_approval_based_paymaster_input, get_approval_based_paymaster_input_for_estimation,
        },
        web3::TransactionReceipt,
        EthNamespaceClient, EthereumProvider, ZksNamespaceClient,
    },
    utils::format_eth,
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

        let final_result = self.send_initial_transfers().await?;
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
        if btc_balance < bitcoin::Amount::from_btc(1.0).unwrap().to_sat() {
            anyhow::bail!(
                "BTC balance on {} is too low to safely perform the loadtest: {} - at least 1 BTC is required",
                master_wallet.btc_address,
                btc_balance
            );
        }
        tracing::info!(
            "Master Account {} L1 BTC balance is {} sats",
            master_wallet.btc_address,
            btc_balance
        );

        Ok(())
    }

    /// Deposits BTC to the master account
    async fn deposit_btc_to_master(&mut self) -> anyhow::Result<()> {
        tracing::info!("Master Account: Depositing BTC");
        let master_wallet = &mut self.pool.btc_master_wallet;

        let deposit_amount = bitcoin::Amount::from_btc(0.1).unwrap().to_sat();

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

        let deposit_amount = bitcoin::Amount::from_btc(0.05).unwrap().to_sat();

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

    /// Distributes BTC to test accounts on L1 and L2
    async fn distribute_btc(&mut self, accounts_to_process: usize) -> anyhow::Result<()> {
        tracing::info!("Master Account: Distributing BTC to test accounts");
        let master_wallet = &mut self.pool.btc_master_wallet;

        let l1_transfer_amount = bitcoin::Amount::from_btc(0.01).unwrap().to_sat();
        let l2_transfer_amount = bitcoin::Amount::from_btc(0.005).unwrap().to_sat();

        for (eth_account, btc_account) in self
            .pool
            .eth_accounts
            .iter()
            .zip(self.pool.btc_accounts.iter())
            .take(accounts_to_process)
        {
            // L1 BTC transfer
            let deposit_response = btc_deposit::deposit(
                l1_transfer_amount,
                eth_account.address(),
                master_wallet.btc_private_key,
                self.config.l1_btc_rpc_address.clone(),
                self.config.l1_btc_rpc_username.clone(),
                self.config.l1_btc_rpc_password.clone(),
            )
            .await;

            match deposit_response {
                Ok(hash) => {
                    tracing::info!(
                        "BTC L1 transfer sent with hash: {} to account {}",
                        hash,
                        eth_account.address()
                    );
                }
                Err(err) => {
                    anyhow::bail!("Failed to transfer BTC on L1: {}", err);
                }
            }

            // L2 BTC deposit
            let deposit_response = btc_deposit::deposit(
                l2_transfer_amount,
                eth_account.address(),
                btc_account.btc_private_key,
                self.config.l1_btc_rpc_address.clone(),
                self.config.l1_btc_rpc_username.clone(),
                self.config.l1_btc_rpc_password.clone(),
            )
            .await;

            match deposit_response {
                Ok(hash) => {
                    tracing::info!(
                        "BTC L2 deposit sent with hash: {} for account {}",
                        hash,
                        eth_account.address()
                    );
                }
                Err(err) => {
                    anyhow::bail!("Failed to deposit BTC to L2: {}", err);
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
    async fn send_initial_transfers(&mut self) -> anyhow::Result<LoadtestResult> {
        tracing::info!("Master Account: Sending initial transfers");
        // How many times we will resend a batch.
        const MAX_RETRIES: usize = 3;

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

        let mut retry_counter = 0;
        let mut accounts_processed = 0;
        let limiters = Arc::new(RequestLimiters::new(config));

        let mut account_tasks = vec![];
        while accounts_processed != accounts_amount {
            if retry_counter > MAX_RETRIES {
                anyhow::bail!("Reached max amount of retries when sending initial transfers");
            }

            let accounts_left = accounts_amount - accounts_processed;
            let max_accounts_per_iter = MAX_OUTSTANDING_NONCE;
            let accounts_to_process = std::cmp::min(accounts_left, max_accounts_per_iter);

            let accounts_to_process = accounts_to_process;

            if let Err(err) = self.send_initial_transfers_inner(accounts_to_process).await {
                tracing::warn!("Iteration of the initial funds distribution failed: {err}");
                retry_counter += 1;
                continue;
            }

            accounts_processed += accounts_to_process;
            tracing::info!("[{accounts_processed}/{accounts_amount}] Accounts processed");

            retry_counter = 0;

            let contract_execution_params = self.execution_config.contract_execution_params.clone();
            // Spawn each account lifespan.
            let main_token = self.l2_main_token;

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
                        main_token,
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

    /// Calculates amount of ETH to be distributed per account in order to make them
    /// able to perform priority operations.
    async fn eth_amount_to_distribute(&self) -> anyhow::Result<U256> {
        let ethereum = self
            .pool
            .eth_master_wallet
            .ethereum(&self.config.l1_rpc_address)
            .await
            .expect("Can't get Ethereum client");

        // Assuming that gas prices on testnets are somewhat stable, we will consider it a constant.
        let average_gas_price = ethereum.query_client().get_gas_price().await?;

        let gas_price_with_priority = average_gas_price + U256::from(DEFAULT_PRIORITY_FEE);

        // TODO (PLA-85): Add gas estimations for deposits in Rust SDK
        let average_l1_to_l2_gas_limit = 5_000_000u32;
        let average_price_for_l1_to_l2_execute = ethereum
            .base_cost(
                average_l1_to_l2_gas_limit.into(),
                REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE as u32,
                Some(gas_price_with_priority),
            )
            .await?;

        Ok(
            gas_price_with_priority * MAX_L1_TRANSACTION_GAS_LIMIT * MAX_L1_TRANSACTIONS
                + average_price_for_l1_to_l2_execute * MAX_L1_TRANSACTIONS,
        )
    }

    /// Returns the amount of funds to be deposited on the main account in L2.
    /// Amount is chosen to be big enough to not worry about precisely calculating the remaining balances on accounts,
    /// but also to not be close to the supported limits in ZKsync.
    fn amount_to_deposit(&self) -> u128 {
        u128::MAX >> 32
    }

    /// Returns the amount of funds to be distributed between accounts on l1.
    fn amount_for_l1_distribution(&self) -> u128 {
        u128::MAX >> 29
    }

    /// Ensures that Ethereum transaction was successfully executed.
    async fn assert_eth_tx_success(&self, receipt: &TransactionReceipt) {
        if receipt.status != Some(1u64.into()) {
            let master_wallet = &self.pool.eth_master_wallet;
            let ethereum = master_wallet
                .ethereum(&self.config.l1_rpc_address)
                .await
                .expect("Can't get Ethereum client");
            let failure_reason = ethereum
                .query_client()
                .failure_reason(receipt.transaction_hash)
                .await
                .expect("Can't connect to the Ethereum node");
            panic!(
                "Ethereum transaction unexpectedly failed.\nReceipt: {:#?}\nFailure reason: {:#?}",
                receipt, failure_reason
            );
        }
    }

    async fn send_initial_transfers_inner(&self, accounts_to_process: usize) -> anyhow::Result<()> {
        let eth_to_distribute = self.eth_amount_to_distribute().await?;
        let master_wallet = &self.pool.eth_master_wallet;

        let l1_transfer_amount =
            self.amount_for_l1_distribution() / self.config.accounts_amount as u128;
        let l2_transfer_amount = self.erc20_transfer_amount();

        // let weight_of_l1_txs = self.execution_config.transaction_weights.l1_transactions
        //     + self.execution_config.transaction_weights.deposit;

        let weight_of_l1_txs = 0.0;

        let paymaster_address = self
            .pool
            .eth_master_wallet
            .provider
            .get_testnet_paymaster()
            .await?
            .expect("No testnet paymaster is set");

        let mut ethereum = master_wallet
            .ethereum(&self.config.l1_rpc_address)
            .await
            .expect("Can't get Ethereum client");
        ethereum.set_confirmation_timeout(ETH_CONFIRMATION_TIMEOUT);
        ethereum.set_polling_interval(ETH_POLLING_INTERVAL);

        // We request nonce each time, so that if one iteration was failed, it will be repeated on the next iteration.
        let mut nonce = Nonce(master_wallet.get_nonce().await?);

        let txs_amount = accounts_to_process * 2 + 1;
        let mut handles = Vec::with_capacity(accounts_to_process);

        // 2 txs per account (1 ERC-20 & 1 ETH transfer).
        let mut eth_txs = Vec::with_capacity(txs_amount * 2);
        let mut eth_nonce = ethereum.client().pending_nonce().await?;

        for account in self.pool.eth_accounts.iter().take(accounts_to_process) {
            let target_address = account.wallet.address();

            // Prior to sending funds in L2, we will send funds in L1 for accounts
            // to be able to perform priority operations.
            // We don't actually care whether transactions will be successful or not; at worst we will not use
            // priority operations in test.

            // If we don't need to send l1 txs we don't need to distribute the funds
            if weight_of_l1_txs != 0.0 {
                let balance = ethereum.query_client().eth_balance(target_address).await?;
                let gas_price = ethereum.query_client().get_gas_price().await?;

                if balance < eth_to_distribute {
                    let options = Options {
                        nonce: Some(eth_nonce),
                        max_fee_per_gas: Some(gas_price * 2),
                        max_priority_fee_per_gas: Some(gas_price * 2),
                        ..Default::default()
                    };
                    let res = ethereum
                        .transfer(
                            ETHEREUM_ADDRESS.to_owned(),
                            eth_to_distribute,
                            target_address,
                            Some(options),
                        )
                        .await
                        .unwrap();
                    eth_nonce += U256::one();
                    eth_txs.push(res);
                }

                let ethereum_erc20_balance = ethereum
                    .erc20_balance(target_address, self.config.main_token)
                    .await?;

                if ethereum_erc20_balance < U256::from(l1_transfer_amount) {
                    let options = Options {
                        nonce: Some(eth_nonce),
                        max_fee_per_gas: Some(gas_price * 2),
                        max_priority_fee_per_gas: Some(gas_price * 2),
                        ..Default::default()
                    };
                    let res = ethereum
                        .transfer(
                            self.config.main_token,
                            U256::from(l1_transfer_amount),
                            target_address,
                            Some(options),
                        )
                        .await?;
                    eth_nonce += U256::one();
                    eth_txs.push(res);
                }
            }

            // And then we will prepare an L2 transaction to send ERC20 token (for transfers and fees).
            let mut builder = master_wallet
                .start_transfer()
                .to(target_address)
                .amount(l2_transfer_amount.into())
                .token(self.l2_main_token)
                .nonce(nonce);

            let paymaster_params = get_approval_based_paymaster_input_for_estimation(
                paymaster_address,
                self.l2_main_token,
                MIN_ALLOWANCE_FOR_PAYMASTER_ESTIMATE.into(),
            );

            let fee = builder.estimate_fee(Some(paymaster_params)).await?;
            builder = builder.fee(fee.clone());

            let paymaster_params = get_approval_based_paymaster_input(
                paymaster_address,
                self.l2_main_token,
                fee.max_total_fee(),
                Vec::new(),
            );
            builder = builder.fee(fee);
            builder = builder.paymaster_params(paymaster_params);

            let handle_erc20 = builder.send().await?;
            handles.push(handle_erc20);

            *nonce += 1;
        }

        // Wait for transactions to be committed, if at least one of them fails,
        // return error.
        for mut handle in handles {
            handle.polling_interval(POLLING_INTERVAL).unwrap();

            let result = handle
                .commit_timeout(COMMIT_TIMEOUT)
                .wait_for_commit()
                .await?;
            if result.status == U64::zero() {
                return Err(anyhow::format_err!("Transfer failed"));
            }
        }

        tracing::info!("Master account: Wait for ethereum txs confirmations, {eth_txs:?}");
        for eth_tx in eth_txs {
            ethereum.wait_for_tx(eth_tx).await?;
        }

        Ok(())
    }

    /// Returns the amount sufficient for wallets to perform many operations.
    fn erc20_transfer_amount(&self) -> u128 {
        let accounts_amount = self.config.accounts_amount;
        let account_balance = self.amount_to_deposit();
        let for_fees = u64::MAX; // Leave some spare funds on the master account for fees.
        let funds_to_distribute = account_balance - u128::from(for_fees);
        funds_to_distribute / accounts_amount as u128
    }
}

async fn deposit_with_attempts(
    ethereum: &EthereumProvider<PrivateKeySigner>,
    to: Address,
    token: Address,
    deposit_amount: U256,
    max_attempts: usize,
) -> anyhow::Result<TransactionReceipt> {
    let nonce = ethereum.client().current_nonce().await.unwrap();
    for attempt in 1..=max_attempts {
        let pending_block_base_fee_per_gas = ethereum
            .query_client()
            .get_pending_block_base_fee_per_gas()
            .await
            .unwrap();

        let max_priority_fee_per_gas = U256::from(DEFAULT_PRIORITY_FEE * 10 * attempt as u64);
        let max_fee_per_gas = U256::from(
            (pending_block_base_fee_per_gas.as_u64() as f64 * (1.0 + 0.1 * attempt as f64)) as u64,
        ) + max_priority_fee_per_gas;

        let options = Options {
            max_fee_per_gas: Some(max_fee_per_gas),
            max_priority_fee_per_gas: Some(max_priority_fee_per_gas),
            nonce: Some(nonce),
            ..Default::default()
        };
        let deposit_tx_hash = ethereum
            .deposit(token, deposit_amount, to, None, None, Some(options))
            .await?;

        tracing::info!("Deposit with tx_hash {deposit_tx_hash:?}");

        // Wait for the corresponding priority operation to be committed in ZKsync.
        match ethereum.wait_for_tx(deposit_tx_hash).await {
            Ok(eth_receipt) => {
                return Ok(eth_receipt);
            }
            Err(err) => {
                tracing::error!("Deposit error: {err}");
            }
        };
    }
    anyhow::bail!("Max attempts limits reached");
}
