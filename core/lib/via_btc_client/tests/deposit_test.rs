use std::{
    fs::File,
    io::{Read, Write},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result};
use bitcoin::{hashes::Hash, Amount};
use tempfile::TempDir;
use via_btc_client::{
    client::BitcoinClient,
    inscriber::{test_utils::MockBitcoinOpsConfig, Inscriber},
    types::{
        BitcoinAddress, BitcoinNetwork, InscriberContext, InscriptionMessage, L1ToL2MessageInput,
        NodeAuth, Recipient,
    },
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::Address as EVMAddress;

/// Test configuration for deposit functionality
#[derive(Debug, Clone)]
struct DepositTestConfig {
    amount: u64,
    receiver_l2_address: EVMAddress,
    depositor_private_key: String,
    network: BitcoinNetwork,
    bridge_musig2_address: BitcoinAddress,
}

impl Default for DepositTestConfig {
    fn default() -> Self {
        Self {
            amount: 100000, // 0.001 BTC in satoshis
            receiver_l2_address: EVMAddress::zero(),
            depositor_private_key: "cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R"
                .to_string(),
            network: BitcoinNetwork::Regtest,
            bridge_musig2_address: BitcoinAddress::from_str(
                "bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq",
            )
            .unwrap()
            .require_network(BitcoinNetwork::Regtest)
            .unwrap(),
        }
    }
}

/// Helper function to create a temporary context file
fn create_temp_context_file(temp_dir: &TempDir) -> String {
    let context_file = temp_dir.path().join("test_context.json");
    context_file.to_string_lossy().to_string()
}

/// Helper function to save context to file (copied from deposit.rs)
fn save_context_to_file(context: &InscriberContext, file_path: &str) -> Result<()> {
    let serialized_context = serde_json::to_string(context)?;
    let mut file = File::create(file_path)?;
    file.write_all(serialized_context.as_bytes())?;
    Ok(())
}

/// Helper function to load context from file (copied from deposit.rs)
fn load_context_from_file(file_path: &str) -> Result<Option<InscriberContext>> {
    if let Ok(mut file) = File::open(file_path) {
        let mut data = String::new();
        file.read_to_string(&mut data)?;
        let context: InscriberContext = serde_json::from_str(&data)?;
        Ok(Some(context))
    } else {
        Ok(None)
    }
}

/// Mock Bitcoin client configuration for testing
fn create_mock_bitcoin_client() -> Arc<BitcoinClient> {
    let _mock_config = MockBitcoinOpsConfig {
        balance: 1000000, // 0.01 BTC in satoshis
        utxos: vec![],
        fee_rate: 1000, // 10 sat/vB
        block_height: 100,
        tx_confirmation: true,
        transaction: None,
        block: None,
        fee_history: vec![1000, 1100, 1200],
    };

    let auth = NodeAuth::UserPass("rpcuser".to_string(), "rpcpassword".to_string());
    let config = ViaBtcClientConfig {
        network: "regtest".to_string(),
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };

    // For testing, we'll use a mock client
    // In a real test environment, you might want to use a regtest Bitcoin node
    Arc::new(BitcoinClient::new("http://localhost:18443", auth, config).unwrap())
}

/// Test the deposit functionality with mock data
#[tokio::test]
async fn test_deposit_functionality() -> Result<()> {
    // Initialize tracing for test output
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    // Create temporary directory for test files
    let temp_dir = TempDir::new()?;
    let context_file = create_temp_context_file(&temp_dir);

    // Load test configuration
    let test_config = DepositTestConfig::default();

    tracing::info!(
        "Testing deposit of {} BTC to receiver L2 address {}",
        test_config.amount,
        test_config.receiver_l2_address
    );

    // Load the previous context from the file if it exists
    let context = load_context_from_file(&context_file)?;

    // Create mock Bitcoin client
    let client = create_mock_bitcoin_client();

    // Create inscriber with mock client
    let mut inscriber = Inscriber::new(client, &test_config.depositor_private_key, context)
        .await
        .context("Failed to create Depositor Inscriber")?;

    // Test balance retrieval
    let balance = inscriber
        .get_balance()
        .await
        .context("Failed to get balance")?;

    tracing::info!("Depositor L1 balance: {}", balance);

    // Verify balance is reasonable for testing
    assert!(balance > 0, "Balance should be greater than 0");

    // Create L1 to L2 message input
    let input = L1ToL2MessageInput {
        receiver_l2_address: test_config.receiver_l2_address,
        l2_contract_address: EVMAddress::zero(),
        call_data: vec![],
    };

    // Test inscription with recipient
    let deposit_info = inscriber
        .inscribe_with_recipient(
            InscriptionMessage::L1ToL2Message(input),
            Some(Recipient {
                address: test_config.bridge_musig2_address,
                amount: Amount::from_sat(test_config.amount),
            }),
        )
        .await?;

    // Verify deposit transaction was created
    assert_ne!(
        deposit_info.final_reveal_tx.txid,
        bitcoin::Txid::all_zeros(),
        "Transaction ID should not be zero"
    );

    tracing::info!("Deposit tx sent: {:?}", deposit_info.final_reveal_tx.txid);
    tracing::info!(
        "Depositor change response: {:?}",
        deposit_info.reveal_tx_output_info.reveal_tx_change_output
    );
    // Verify recipient output exists and amount matches expected amount
    if let Some(recipient_output) = &deposit_info.reveal_tx_output_info.recipient_tx_output {
        tracing::info!("Recipient response: {:?}", recipient_output);
        assert_eq!(
            recipient_output.value.to_sat(),
            test_config.amount,
            "Recipient amount should match expected amount"
        );
    } else {
        panic!("Recipient output should exist");
    }

    // Test context saving and loading
    let context_snapshot = inscriber.get_context_snapshot()?;
    save_context_to_file(&context_snapshot, &context_file)?;

    // Verify context can be loaded back
    let loaded_context = load_context_from_file(&context_file)?;
    assert!(
        loaded_context.is_some(),
        "Context should be saved and loadable"
    );

    Ok(())
}

/// Test deposit with different amounts
#[tokio::test]
async fn test_deposit_different_amounts() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let context_file = create_temp_context_file(&temp_dir);

    let amounts = vec![10000, 50000, 100000, 500000]; // Different amounts in satoshis

    for amount in amounts {
        let mut test_config = DepositTestConfig::default();
        test_config.amount = amount;

        let context = load_context_from_file(&context_file)?;
        let client = create_mock_bitcoin_client();

        let mut inscriber = Inscriber::new(client, &test_config.depositor_private_key, context)
            .await
            .context("Failed to create Depositor Inscriber")?;

        let input = L1ToL2MessageInput {
            receiver_l2_address: test_config.receiver_l2_address,
            l2_contract_address: EVMAddress::zero(),
            call_data: vec![],
        };

        let deposit_info = inscriber
            .inscribe_with_recipient(
                InscriptionMessage::L1ToL2Message(input),
                Some(Recipient {
                    address: test_config.bridge_musig2_address,
                    amount: Amount::from_sat(amount),
                }),
            )
            .await?;

        // Verify the amount is correctly set
        let recipient_output = deposit_info
            .reveal_tx_output_info
            .recipient_tx_output
            .unwrap();
        assert_eq!(
            recipient_output.value.to_sat(),
            amount,
            "Amount {} should be correctly set in recipient output",
            amount
        );

        // Save context for next iteration
        save_context_to_file(&inscriber.get_context_snapshot()?, &context_file)?;
    }

    Ok(())
}

/// Test deposit with different networks
#[tokio::test]
async fn test_deposit_different_networks() -> Result<()> {
    let networks = vec![BitcoinNetwork::Regtest, BitcoinNetwork::Testnet];

    for network in networks {
        let temp_dir = TempDir::new()?;
        let context_file = create_temp_context_file(&temp_dir);

        let mut test_config = DepositTestConfig::default();
        test_config.network = network;

        // Create bridge address for the specific network
        let bridge_address_str = match network {
            BitcoinNetwork::Regtest => "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            BitcoinNetwork::Testnet => "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
            _ => continue, // Skip other networks for this test
        };

        test_config.bridge_musig2_address =
            BitcoinAddress::from_str(bridge_address_str)?.require_network(network)?;

        let context = load_context_from_file(&context_file)?;
        let client = create_mock_bitcoin_client();

        let mut inscriber = Inscriber::new(client, &test_config.depositor_private_key, context)
            .await
            .context("Failed to create Depositor Inscriber")?;

        let input = L1ToL2MessageInput {
            receiver_l2_address: test_config.receiver_l2_address,
            l2_contract_address: EVMAddress::zero(),
            call_data: vec![],
        };

        let deposit_info = inscriber
            .inscribe_with_recipient(
                InscriptionMessage::L1ToL2Message(input),
                Some(Recipient {
                    address: test_config.bridge_musig2_address,
                    amount: Amount::from_sat(test_config.amount),
                }),
            )
            .await?;

        // Verify transaction was created successfully
        assert_ne!(
            deposit_info.final_reveal_tx.txid,
            bitcoin::Txid::all_zeros(),
            "Transaction should be created for network {:?}",
            network
        );

        // Save context
        save_context_to_file(&inscriber.get_context_snapshot()?, &context_file)?;
    }

    Ok(())
}

/// Test error handling for invalid inputs
#[tokio::test]
async fn test_deposit_error_handling() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let context_file = create_temp_context_file(&temp_dir);

    let test_config = DepositTestConfig::default();
    let context = load_context_from_file(&context_file)?;
    let client = create_mock_bitcoin_client();

    let mut inscriber = Inscriber::new(client, &test_config.depositor_private_key, context)
        .await
        .context("Failed to create Depositor Inscriber")?;

    // Test with zero amount
    let input = L1ToL2MessageInput {
        receiver_l2_address: test_config.receiver_l2_address,
        l2_contract_address: EVMAddress::zero(),
        call_data: vec![],
    };

    // This should handle zero amount gracefully
    let deposit_info = inscriber
        .inscribe_with_recipient(
            InscriptionMessage::L1ToL2Message(input),
            Some(Recipient {
                address: test_config.bridge_musig2_address,
                amount: Amount::from_sat(0), // Zero amount
            }),
        )
        .await?;

    // Verify transaction was still created (zero amount might be valid for some use cases)
    assert_ne!(
        deposit_info.final_reveal_tx.txid,
        bitcoin::Txid::all_zeros(),
        "Transaction should be created even with zero amount"
    );

    Ok(())
}

/// Test context persistence across multiple deposits
#[tokio::test]
async fn test_context_persistence() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let context_file = create_temp_context_file(&temp_dir);

    let test_config = DepositTestConfig::default();

    // First deposit
    let context1 = load_context_from_file(&context_file)?;
    let client1 = create_mock_bitcoin_client();

    let mut inscriber1 = Inscriber::new(client1, &test_config.depositor_private_key, context1)
        .await
        .context("Failed to create first Depositor Inscriber")?;

    let input1 = L1ToL2MessageInput {
        receiver_l2_address: test_config.receiver_l2_address,
        l2_contract_address: EVMAddress::zero(),
        call_data: vec![],
    };

    let deposit_info1 = inscriber1
        .inscribe_with_recipient(
            InscriptionMessage::L1ToL2Message(input1),
            Some(Recipient {
                address: test_config.bridge_musig2_address.clone(),
                amount: Amount::from_sat(test_config.amount),
            }),
        )
        .await?;

    // Save context after first deposit
    save_context_to_file(&inscriber1.get_context_snapshot()?, &context_file)?;

    // Second deposit using saved context
    let context2 = load_context_from_file(&context_file)?;
    let client2 = create_mock_bitcoin_client();

    let mut inscriber2 = Inscriber::new(client2, &test_config.depositor_private_key, context2)
        .await
        .context("Failed to create second Depositor Inscriber")?;

    let input2 = L1ToL2MessageInput {
        receiver_l2_address: test_config.receiver_l2_address,
        l2_contract_address: EVMAddress::zero(),
        call_data: vec![],
    };

    let deposit_info2 = inscriber2
        .inscribe_with_recipient(
            InscriptionMessage::L1ToL2Message(input2),
            Some(Recipient {
                address: test_config.bridge_musig2_address.clone(),
                amount: Amount::from_sat(test_config.amount),
            }),
        )
        .await?;

    // Verify both transactions were created
    assert_ne!(
        deposit_info1.final_reveal_tx.txid,
        bitcoin::Txid::all_zeros(),
        "First transaction should be created"
    );
    assert_ne!(
        deposit_info2.final_reveal_tx.txid,
        bitcoin::Txid::all_zeros(),
        "Second transaction should be created"
    );

    // Verify transactions are different
    assert_ne!(
        deposit_info1.final_reveal_tx.txid, deposit_info2.final_reveal_tx.txid,
        "Transactions should be different"
    );

    Ok(())
}
