use std::str::FromStr;

use bitcoin::Network;
use via_btc_client::types::BitcoinAddress;
use zksync_eth_signer::EthereumSigner;
use zksync_types::{
    ethabi, fee::Fee, l2::L2Tx, transaction_request::PaymasterParams, Address, Nonce,
    L2_BASE_TOKEN_ADDRESS, U256,
};

use crate::sdk::{
    error::ClientError,
    operations::{ExecuteContractBuilder, SyncTransactionHandle},
    wallet::Wallet,
    EthNamespaceClient, ZksNamespaceClient,
};

pub struct WithdrawBuilder<'a, S: EthereumSigner, P> {
    wallet: &'a Wallet<S, P>,
    to: Option<BitcoinAddress>,
    amount: Option<U256>,
    fee: Option<Fee>,
    nonce: Option<Nonce>,
    bridge: Option<Address>,
    paymaster_params: Option<PaymasterParams>,
}

impl<'a, S, P> WithdrawBuilder<'a, S, P>
where
    S: EthereumSigner,
    P: ZksNamespaceClient + EthNamespaceClient + Sync,
{
    /// Initializes a withdraw transaction building process.
    pub fn new(wallet: &'a Wallet<S, P>) -> Self {
        Self {
            wallet,
            to: None,
            amount: None,
            fee: None,
            nonce: None,
            bridge: None,
            paymaster_params: None,
        }
    }

    async fn get_execute_builder(&self) -> Result<ExecuteContractBuilder<'_, S, P>, ClientError> {
        let to = self
            .to
            .clone()
            .ok_or_else(|| ClientError::MissingRequiredField("to".into()))?;
        let amount = self
            .amount
            .ok_or_else(|| ClientError::MissingRequiredField("amount".into()))?;

        let contract_address = L2_BASE_TOKEN_ADDRESS;

        let calldata_params = vec![ethabi::ParamType::Bytes];
        let mut calldata = ethabi::short_signature("withdraw", &calldata_params).to_vec();
        let mut to_bytes =
            ethabi::encode(&[ethabi::Token::Bytes(to.to_string().as_bytes().to_vec())]);
        calldata.append(&mut to_bytes);

        let value = amount;

        let paymaster_params = self.paymaster_params.clone().unwrap_or_default();

        let mut builder = ExecuteContractBuilder::new(self.wallet)
            .contract_address(contract_address)
            .calldata(calldata)
            .value(value)
            .paymaster_params(paymaster_params);

        if let Some(fee) = self.fee.clone() {
            builder = builder.fee(fee);
        }
        if let Some(nonce) = self.nonce {
            builder = builder.nonce(nonce);
        }

        Ok(builder)
    }

    /// Directly returns the signed withdraw transaction for the subsequent usage.
    pub async fn tx(self) -> Result<L2Tx, ClientError> {
        let builder = self.get_execute_builder().await?;
        builder.tx().await
    }

    /// Sends the transaction, returning the handle for its awaiting.
    pub async fn send(self) -> Result<SyncTransactionHandle<'a, P>, ClientError> {
        let wallet = self.wallet;
        let tx = self.tx().await?;

        wallet.send_transaction(tx).await
    }

    /// Set the withdrawal amount.
    ///
    /// For more details, see [utils](../utils/index.html) functions.
    pub fn amount(mut self, amount: U256) -> Self {
        self.amount = Some(amount);
        self
    }

    /// Set the fee amount.
    ///
    /// For more details, see [utils](../utils/index.html) functions.
    pub fn fee(mut self, fee: Fee) -> Self {
        self.fee = Some(fee);

        self
    }

    /// Sets the address of Ethereum wallet to withdraw funds to.
    pub fn to(mut self, to: BitcoinAddress) -> Self {
        self.to = Some(to);
        self
    }

    /// Same as `WithdrawBuilder::to`, but accepts a string address value.
    ///
    /// Provided string value must be a correct address in a hexadecimal form,
    /// otherwise an error will be returned.
    pub fn str_to(mut self, to: impl AsRef<str>) -> Result<Self, ClientError> {
        let to: BitcoinAddress = BitcoinAddress::from_str(to.as_ref())
            .map_err(|_| ClientError::IncorrectAddress)?
            .require_network(Network::Regtest)
            .map_err(|_| ClientError::IncorrectAddress)?;

        self.to = Some(to);
        Ok(self)
    }

    /// Sets the transaction nonce.
    pub fn nonce(mut self, nonce: Nonce) -> Self {
        self.nonce = Some(nonce);
        self
    }

    /// Sets the bridge contract to request the withdrawal.
    pub fn bridge(mut self, address: Address) -> Self {
        self.bridge = Some(address);
        self
    }

    /// Sets the paymaster parameters.
    pub fn paymaster_params(mut self, paymaster_params: PaymasterParams) -> Self {
        self.paymaster_params = Some(paymaster_params);
        self
    }

    pub async fn estimate_fee(
        &self,
        paymaster_params: Option<PaymasterParams>,
    ) -> Result<Fee, ClientError> {
        let builder = self.get_execute_builder().await?;
        builder.estimate_fee(paymaster_params).await
    }
}
