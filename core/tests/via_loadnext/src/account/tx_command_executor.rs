use std::time::Instant;

use via_btc_client::{
    client::BitcoinClient,
    traits::BitcoinOps,
    types::{BitcoinError, BitcoinNetwork, NodeAuth},
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::{
    api::{BlockNumber, TransactionReceipt},
    l2::L2Tx,
    Address, H256, L2_BASE_TOKEN_ADDRESS, U256,
};

use super::btc_deposit;
use crate::{
    account::{AccountLifespan, ExecutionType},
    command::{IncorrectnessModifier, TxCommand, TxType},
    constants::MIN_ALLOWANCE_FOR_PAYMASTER_ESTIMATE,
    corrupted_tx::Corrupted,
    report::ReportLabel,
    sdk::{
        error::ClientError,
        ethabi,
        utils::{
            get_approval_based_paymaster_input, get_approval_based_paymaster_input_for_estimation,
        },
        EthNamespaceClient,
    },
    utils::format_gwei,
};

impl From<BitcoinError> for ClientError {
    fn from(err: BitcoinError) -> Self {
        ClientError::NetworkError(err.to_string())
    }
}

#[derive(Debug)]
pub enum SubmitResult {
    TxHash(H256),
    ReportLabel(ReportLabel),
}

impl AccountLifespan {
    pub(super) async fn execute_tx_command(
        &mut self,
        command: &TxCommand,
    ) -> Result<SubmitResult, ClientError> {
        match command.command_type {
            TxType::Withdraw => self.execute_withdraw(command).await,
            TxType::Deposit => self.execute_btc_deposit(command).await,
            TxType::DeployContract => self.execute_deploy_contract(command).await,
            TxType::L2Execute => {
                self.execute_loadnext_contract(command, ExecutionType::L2)
                    .await
            }
        }
    }

    fn tx_creation_error(err: ClientError) -> ClientError {
        // Translate network errors (so operation will be retried), but don't accept other ones.
        // For example, we will retry operation if fee ticker returned an error,
        // but will panic if transaction cannot be signed.
        match err {
            ClientError::NetworkError(_)
            | ClientError::RpcError(_)
            | ClientError::MalformedResponse(_) => err,
            _ => panic!("Transaction should be correct"),
        }
    }

    async fn apply_modifier(&self, tx: L2Tx, modifier: IncorrectnessModifier) -> L2Tx {
        let wallet = &self.eth_wallet.wallet;
        tx.apply_modifier(modifier, &wallet.signer).await
    }

    /// Returns the balances for BTC and the main token on the L1.
    /// This function is used to check whether the L1 operation can be performed or should be
    /// skipped.
    async fn l1_btc_balances(&self) -> Result<U256, ClientError> {
        let wallet = &self.btc_wallet;

        let config = ViaBtcClientConfig {
            network: BitcoinNetwork::Regtest.to_string(),
            external_apis: vec![],
            fee_strategies: vec![],
            use_rpc_for_fee_rate: None,
        };

        let btc_client = BitcoinClient::new(
            &self.config.l1_btc_rpc_address,
            NodeAuth::UserPass(
                self.config.l1_btc_rpc_username.clone(),
                self.config.l1_btc_rpc_password.clone(),
            ),
            config,
        )?;

        let balance = btc_client.get_balance(&wallet.btc_address).await?;
        Ok(U256::from(balance))
    }

    async fn execute_btc_deposit(&self, command: &TxCommand) -> Result<SubmitResult, ClientError> {
        let btc_balance = self.l1_btc_balances().await?;
        if btc_balance.is_zero()
            || btc_balance < command.amount
            || bitcoin::Amount::from_sat(btc_balance.as_u64())
                < bitcoin::Amount::from_btc(0.0001).unwrap()
        {
            // We don't have either funds in L1 to pay for tx or to deposit.
            // It's not a problem with the server, thus we mark this operation as skipped.
            let label = ReportLabel::skipped("No L1 balance");
            return Ok(SubmitResult::ReportLabel(label));
        }

        let deposit_response = btc_deposit::deposit(
            command.amount.as_u64(),
            command.to,
            self.btc_wallet.btc_private_key,
            self.config.bridge_address.clone(),
            self.config.l1_btc_rpc_address.clone(),
            self.config.l1_btc_rpc_username.clone(),
            self.config.l1_btc_rpc_password.clone(),
        )
        .await;

        let _btc_tx_hash = match deposit_response {
            Ok(hash) => hash,
            Err(err) => {
                let reason = format!("Unable to perform a BTC operation. Reason: {err}");
                return Ok(SubmitResult::ReportLabel(ReportLabel::skipped(reason)));
            }
        };

        // TODO: Add validation for canonical tx hash and return the hash if it's valid
        // (for doing this we need to know current priority op id of network)
        // and for knowing that we need to add rpc call to get priority op id
        Ok(SubmitResult::ReportLabel(ReportLabel::done()))
    }

    async fn execute_submit(
        &mut self,
        tx: L2Tx,
        modifier: IncorrectnessModifier,
    ) -> Result<SubmitResult, ClientError> {
        let nonce = tx.nonce();
        let result = match modifier {
            IncorrectnessModifier::IncorrectSignature => {
                let wallet = self.eth_wallet.corrupted_wallet.clone();
                self.submit(modifier, wallet.send_transaction(tx).await)
                    .await
            }
            _ => {
                let wallet = self.eth_wallet.wallet.clone();
                self.submit(modifier, wallet.send_transaction(tx).await)
                    .await
            }
        }?;

        // Update current nonce for future txs
        // If the transaction has a `tx_hash` and is small enough to be included in a block, this tx will change the nonce.
        // We can be sure that the nonce will be changed based on this assumption.
        if let SubmitResult::TxHash(_) = &result {
            self.current_nonce = Some(nonce + 1)
        }

        Ok(result)
    }

    async fn execute_withdraw(&mut self, command: &TxCommand) -> Result<SubmitResult, ClientError> {
        let tx = self.build_withdraw(command).await?;
        self.execute_submit(tx, command.modifier).await
    }

    pub(super) async fn build_withdraw(&self, command: &TxCommand) -> Result<L2Tx, ClientError> {
        let wallet = self.eth_wallet.wallet.clone();

        let mut builder = wallet
            .start_withdraw()
            .to(command.to_btc.clone().unwrap())
            .amount(command.amount);

        let paymaster_approval = if self.config.use_paymaster {
            Some(get_approval_based_paymaster_input_for_estimation(
                self.paymaster_address,
                L2_BASE_TOKEN_ADDRESS,
                MIN_ALLOWANCE_FOR_PAYMASTER_ESTIMATE.into(),
            ))
        } else {
            None
        };

        let fee = builder.estimate_fee(paymaster_approval).await?;
        builder = builder.fee(fee.clone());

        if self.config.use_paymaster {
            let paymaster_params = get_approval_based_paymaster_input(
                self.paymaster_address,
                L2_BASE_TOKEN_ADDRESS,
                fee.max_total_fee(),
                Vec::new(),
            );
            builder = builder.paymaster_params(paymaster_params);
        };

        if let Some(nonce) = self.current_nonce {
            builder = builder.nonce(nonce);
        }
        builder = builder.fee(fee);

        let tx = builder.tx().await.map_err(Self::tx_creation_error)?;

        Ok(self.apply_modifier(tx, command.modifier).await)
    }

    async fn execute_deploy_contract(
        &mut self,
        command: &TxCommand,
    ) -> Result<SubmitResult, ClientError> {
        let tx = self.build_deploy_loadnext_contract(command).await?;
        self.execute_submit(tx, command.modifier).await
    }

    async fn build_deploy_loadnext_contract(
        &self,
        command: &TxCommand,
    ) -> Result<L2Tx, ClientError> {
        let wallet = self.eth_wallet.wallet.clone();
        let constructor_calldata = ethabi::encode(&[ethabi::Token::Uint(U256::from(
            self.contract_execution_params.reads,
        ))]);

        let mut builder = wallet
            .start_deploy_contract()
            .bytecode(self.eth_wallet.test_contract.bytecode.to_vec())
            .constructor_calldata(constructor_calldata);

        let paymaster_approval = if self.config.use_paymaster {
            Some(get_approval_based_paymaster_input_for_estimation(
                self.paymaster_address,
                L2_BASE_TOKEN_ADDRESS,
                MIN_ALLOWANCE_FOR_PAYMASTER_ESTIMATE.into(),
            ))
        } else {
            None
        };

        let fee = builder.estimate_fee(paymaster_approval).await?;
        builder = builder.fee(fee.clone());

        if self.config.use_paymaster {
            let paymaster_params = get_approval_based_paymaster_input(
                self.paymaster_address,
                L2_BASE_TOKEN_ADDRESS,
                fee.max_total_fee(),
                Vec::new(),
            );
            builder = builder.paymaster_params(paymaster_params);
        };
        builder = builder.fee(fee);

        if let Some(nonce) = self.current_nonce {
            builder = builder.nonce(nonce);
        }

        let tx = builder.tx().await.map_err(Self::tx_creation_error)?;

        Ok(self.apply_modifier(tx, command.modifier).await)
    }

    async fn execute_loadnext_contract(
        &mut self,
        command: &TxCommand,
        execution_type: ExecutionType,
    ) -> Result<SubmitResult, ClientError> {
        let Some(&contract_address) = self.eth_wallet.deployed_contract_address.get() else {
            let label =
                ReportLabel::skipped("Account haven't successfully deployed a contract yet");
            return Ok(SubmitResult::ReportLabel(label));
        };

        match execution_type {
            ExecutionType::L1 => {
                let label = ReportLabel::skipped("L1 execution is not supported yet");
                Ok(SubmitResult::ReportLabel(label))
            }

            ExecutionType::L2 => {
                let mut started_at = Instant::now();
                let tx = self
                    .build_execute_loadnext_contract(command, contract_address)
                    .await?;
                tracing::trace!(
                    "Account {:?}: execute_loadnext_contract: tx built in {:?}",
                    self.eth_wallet.wallet.address(),
                    started_at.elapsed()
                );
                started_at = Instant::now();
                let result = self.execute_submit(tx, command.modifier).await;
                tracing::trace!(
                    "Account {:?}: execute_loadnext_contract: tx executed in {:?}",
                    self.eth_wallet.wallet.address(),
                    started_at.elapsed()
                );
                result
            }
        }
    }

    fn prepare_calldata_for_loadnext_contract(&self) -> Vec<u8> {
        let contract = &self.eth_wallet.test_contract.abi;
        let function = contract.function("execute").unwrap();
        function
            .encode_input(&vec![
                ethabi::Token::Uint(U256::from(self.contract_execution_params.reads)),
                ethabi::Token::Uint(U256::from(self.contract_execution_params.initial_writes)),
                ethabi::Token::Uint(U256::from(self.contract_execution_params.hashes)),
                ethabi::Token::Uint(U256::from(self.contract_execution_params.events)),
                ethabi::Token::Uint(U256::from(self.contract_execution_params.recursive_calls)),
                ethabi::Token::Uint(U256::from(self.contract_execution_params.deploys)),
            ])
            .expect("failed to encode parameters when creating calldata")
    }

    async fn build_execute_loadnext_contract(
        &mut self,
        command: &TxCommand,
        contract_address: Address,
    ) -> Result<L2Tx, ClientError> {
        let wallet = &self.eth_wallet.wallet;

        let calldata = self.prepare_calldata_for_loadnext_contract();
        let mut builder = wallet
            .start_execute_contract()
            .calldata(calldata)
            .contract_address(contract_address)
            .factory_deps(self.eth_wallet.test_contract.factory_deps());

        let paymaster_approval = if self.config.use_paymaster {
            Some(get_approval_based_paymaster_input_for_estimation(
                self.paymaster_address,
                L2_BASE_TOKEN_ADDRESS,
                MIN_ALLOWANCE_FOR_PAYMASTER_ESTIMATE.into(),
            ))
        } else {
            None
        };

        let fee = builder.estimate_fee(paymaster_approval).await?;

        tracing::trace!(
            "Account {:?}: fee estimated. Max total fee: {}, gas limit: {}gas; Max gas price: {}WEI, \
             Gas per pubdata: {:?}gas",
            self.eth_wallet.wallet.address(),
            format_gwei(fee.max_total_fee()),
            fee.gas_limit,
            fee.max_fee_per_gas,
            fee.gas_per_pubdata_limit
        );
        builder = builder.fee(fee.clone());

        if self.config.use_paymaster {
            let paymaster_params = get_approval_based_paymaster_input(
                self.paymaster_address,
                L2_BASE_TOKEN_ADDRESS,
                fee.max_total_fee(),
                Vec::new(),
            );
            builder = builder.paymaster_params(paymaster_params);
        };
        builder = builder.fee(fee);

        if let Some(nonce) = self.current_nonce {
            builder = builder.nonce(nonce);
        }

        let tx = builder.tx().await.map_err(Self::tx_creation_error)?;

        Ok(self.apply_modifier(tx, command.modifier).await)
    }

    pub(crate) async fn get_tx_receipt_for_committed_block(
        &mut self,
        tx_hash: H256,
    ) -> Result<Option<TransactionReceipt>, ClientError> {
        let response = self
            .eth_wallet
            .wallet
            .provider
            .get_transaction_receipt(tx_hash)
            .await?;

        let Some(receipt) = response else {
            return Ok(None);
        };

        let block_number = receipt.block_number;

        let response = self
            .eth_wallet
            .wallet
            .provider
            .get_block_by_number(BlockNumber::Committed, false)
            .await?;
        if let Some(received_number) = response.map(|block| block.number) {
            if block_number <= received_number {
                return Ok(Some(receipt));
            }
        }
        Ok(None)
    }
}
