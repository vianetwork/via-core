use std::{collections::VecDeque, convert::TryFrom, sync::Arc, time::Duration};

use anyhow::Context as _;
use once_cell::sync::OnceCell;
use rand::Rng;
use tokio::time::timeout;
use via_btc_client::types::{
    BitcoinAddress, BitcoinNetwork, BitcoinPrivateKey, BitcoinSecp256k1, CompressedPublicKey,
};
use zksync_eth_signer::PrivateKeySigner;
use zksync_test_contracts::TestContract;
use zksync_types::{Address, K256PrivateKey, L2ChainId, H256};
use zksync_web3_decl::client::{Client, L2};

use crate::{
    config::LoadtestConfig,
    corrupted_tx::CorruptedSigner,
    rng::{LoadtestRng, Random},
    sdk::{signer::Signer, Wallet, ZksNamespaceClient},
};

/// An alias to [`zksync::Wallet`] with HTTP client. Wrapped in `Arc` since
/// the client cannot be cloned due to limitations in jsonrpsee.
pub type SyncWallet = Arc<Wallet<PrivateKeySigner, Client<L2>>>;
pub type CorruptedSyncWallet = Arc<Wallet<CorruptedSigner, Client<L2>>>;

/// Thread-safe pool of the addresses of accounts used in the loadtest.
#[derive(Debug, Clone)]
pub struct AddressPool {
    evm_addresses: Arc<Vec<Address>>,
    btc_addresses: Arc<Vec<BitcoinAddress>>,
}

impl AddressPool {
    pub fn new(evm_addresses: Vec<Address>, btc_addresses: Vec<BitcoinAddress>) -> Self {
        Self {
            evm_addresses: Arc::new(evm_addresses),
            btc_addresses: Arc::new(btc_addresses),
        }
    }

    /// Randomly chooses one of the addresses stored in the pool.
    pub fn random_evm_address(&self, rng: &mut LoadtestRng) -> Address {
        let index = rng.gen_range(0..self.evm_addresses.len());
        self.evm_addresses[index]
    }

    pub fn random_btc_address(&self, rng: &mut LoadtestRng) -> BitcoinAddress {
        let index = rng.gen_range(0..self.btc_addresses.len());
        self.btc_addresses[index].clone()
    }
}

/// Credentials for a test account.
/// Currently we support only EOA accounts.
#[derive(Debug, Clone)]
pub struct AccountCredentials {
    /// Bitcoin private key.
    pub btc_pk: BitcoinPrivateKey,
    /// Bitcoin address derived from the private key.
    pub btc_address: BitcoinAddress,
    /// Ethereum private key.
    pub eth_pk: K256PrivateKey,
    /// Ethereum address derived from the private key.
    pub evm_address: Address,
}

impl Random for AccountCredentials {
    fn random(rng: &mut LoadtestRng) -> Self {
        let secp = BitcoinSecp256k1::Secp256k1::new();
        let btc_pk = BitcoinPrivateKey::generate(BitcoinNetwork::Regtest);

        let compressed_pk = CompressedPublicKey::from_private_key(&secp, &btc_pk).unwrap();

        let btc_address = BitcoinAddress::p2wpkh(&compressed_pk, BitcoinNetwork::Regtest);

        let eth_pk = K256PrivateKey::random_using(rng);
        let evm_address = eth_pk.address();

        Self {
            btc_pk,
            btc_address,
            eth_pk,
            evm_address,
        }
    }
}

/// Type that contains the data required for the test wallet to operate.
#[derive(Debug, Clone)]
pub struct TestWallet {
    /// Pre-initialized wallet object.
    pub wallet: SyncWallet,
    /// Wallet with corrupted signer.
    pub corrupted_wallet: CorruptedSyncWallet,
    /// Contract bytecode and calldata to be used for sending `Execute` transactions.
    pub test_contract: &'static TestContract,
    /// Address of the deployed contract to be used for sending
    /// `Execute` transaction.
    pub deployed_contract_address: Arc<OnceCell<Address>>,
    /// RNG object derived from a common loadtest seed and the wallet private key.
    pub rng: LoadtestRng,
}

#[derive(Debug, Clone)]
pub struct BtcAccount {
    pub btc_private_key: BitcoinPrivateKey,
    pub btc_address: BitcoinAddress,
}

/// Pool of accounts to be used in the test.
/// Each account is represented as `zksync::Wallet` in order to provide convenient interface of interaction with ZKsync.
#[derive(Debug)]
pub struct AccountPool {
    /// Main wallet that will be used to initialize all the test wallets.
    pub eth_master_wallet: SyncWallet,
    /// Main wallet that will be used to initialize all the test wallets.
    pub btc_master_wallet: BtcAccount,
    /// Collection of test wallets and their Ethereum private keys.
    pub eth_accounts: VecDeque<TestWallet>,
    /// Collection of test wallets and their Bitcoin private keys.
    pub btc_accounts: VecDeque<BtcAccount>,
    /// Pool of addresses of the test accounts.
    pub addresses: AddressPool,
}

impl AccountPool {
    /// Generates all the required test accounts and prepares `Wallet` objects.
    pub async fn new(config: &LoadtestConfig) -> anyhow::Result<Self> {
        let l2_chain_id = L2ChainId::try_from(config.l2_chain_id)
            .map_err(|err| anyhow::anyhow!("invalid L2 chain ID: {err}"))?;
        // Create a client for pinging the RPC.
        let client = Client::http(
            config
                .l2_rpc_address
                .parse()
                .context("invalid L2 RPC URL")?,
        )?
        .for_network(l2_chain_id.into())
        .build();
        // Perform a health check: check whether ZKsync server is alive.
        let mut server_alive = false;
        for _ in 0usize..3 {
            if let Ok(Ok(_)) = timeout(Duration::from_secs(3), client.get_main_contract()).await {
                server_alive = true;
                break;
            }
        }
        if !server_alive {
            anyhow::bail!("Via server does not respond. Please check RPC address and whether server is launched");
        }

        let test_contract = TestContract::load_test();

        let eth_master_wallet = {
            let eth_private_key: H256 = config
                .eth_master_wallet_pk
                .parse()
                .context("cannot parse master wallet private key")?;
            let eth_private_key = K256PrivateKey::from_bytes(eth_private_key)?;
            let address = eth_private_key.address();
            let eth_signer = PrivateKeySigner::new(eth_private_key);
            let signer = Signer::new(eth_signer, address, l2_chain_id);
            Arc::new(Wallet::with_http_client(&config.l2_rpc_address, signer).unwrap())
        };

        let btc_master_wallet = {
            let btc_wif = config.btc_master_wallet_pk.clone();

            let btc_private_key = BitcoinPrivateKey::from_wif(&btc_wif)?;

            let secp = BitcoinSecp256k1::Secp256k1::new();
            let compressed_pk =
                CompressedPublicKey::from_private_key(&secp, &btc_private_key).unwrap();
            let btc_address = BitcoinAddress::p2wpkh(&compressed_pk, BitcoinNetwork::Regtest);

            BtcAccount {
                btc_private_key,
                btc_address,
            }
        };

        let mut rng = LoadtestRng::new_generic(config.seed.clone());
        tracing::info!("Using RNG with master seed: {}", rng.seed_hex());

        let group_size = config.accounts_group_size;
        let accounts_amount = config.accounts_amount;
        anyhow::ensure!(
            group_size <= accounts_amount,
            "Accounts group size is expected to be less than or equal to accounts amount"
        );

        let mut eth_accounts = VecDeque::with_capacity(accounts_amount);
        let mut btc_accounts = VecDeque::with_capacity(accounts_amount);
        let mut eth_addresses = Vec::with_capacity(accounts_amount);
        let mut btc_addresses = Vec::with_capacity(accounts_amount);

        for i in (0..accounts_amount).step_by(group_size) {
            let range_end = (i + group_size).min(accounts_amount);
            // The next group shares the contract address.
            let deployed_contract_address = Arc::new(OnceCell::new());

            for _ in i..range_end {
                let credentials = AccountCredentials::random(&mut rng);
                let private_key_bytes = credentials.eth_pk.expose_secret().secret_bytes();
                let eth_signer = PrivateKeySigner::new(credentials.eth_pk);
                let address = credentials.evm_address;
                let signer = Signer::new(eth_signer, address, l2_chain_id);

                let corrupted_eth_signer = CorruptedSigner::new(address);
                let corrupted_signer = Signer::new(corrupted_eth_signer, address, l2_chain_id);

                let wallet = Wallet::with_http_client(&config.l2_rpc_address, signer).unwrap();
                let corrupted_wallet =
                    Wallet::with_http_client(&config.l2_rpc_address, corrupted_signer).unwrap();

                eth_addresses.push(wallet.address());
                btc_addresses.push(credentials.btc_address.clone());
                let account = TestWallet {
                    wallet: Arc::new(wallet),
                    corrupted_wallet: Arc::new(corrupted_wallet),
                    test_contract,
                    deployed_contract_address: deployed_contract_address.clone(),
                    rng: rng.derive(private_key_bytes),
                };
                eth_accounts.push_back(account);

                let btc_account = BtcAccount {
                    btc_private_key: credentials.btc_pk,
                    btc_address: credentials.btc_address,
                };
                btc_accounts.push_back(btc_account);
            }
        }

        Ok(Self {
            eth_master_wallet,
            btc_master_wallet,
            eth_accounts,
            btc_accounts,
            addresses: AddressPool::new(eth_addresses, btc_addresses),
        })
    }
}
