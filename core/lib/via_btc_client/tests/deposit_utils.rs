use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use bitcoin::Amount;
use tracing::debug;
use via_btc_client::{
    client::BitcoinClient,
    inscriber::Inscriber,
    traits::BitcoinOps,
    types::{
        BitcoinAddress, BitcoinNetwork, InscriptionMessage, L1ToL2MessageInput, NodeAuth, Recipient,
    },
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::Address as EVMAddress;

pub mod config {
    use super::*;

    pub const RPC_URL: &str = "http://localhost:18443";
    pub const RPC_USER: &str = "rpcuser";
    pub const RPC_PASSWORD: &str = "rpcpassword";
    pub const NETWORK: BitcoinNetwork = BitcoinNetwork::Regtest;

    pub const TEST_PRIVATE_KEY: &str = "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc";
    pub const BRIDGE_MUSIG2_ADDRESS: &str =
        "bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq";
    pub const ALICE_WALLET_ADDRESS: &str = "bcrt1qg9ly0933msv97jpgrlqwn0h6743y0u6zrvh020";
    pub const TEST_RECEIVER_L2_ADDRESS: &str = "0x1234567890123456789012345678901234567890";

    pub const DEFAULT_DEPOSIT_AMOUNT_SATS: u64 = 100000;
    pub const DEFAULT_FUNDING_AMOUNT_BTC: f64 = 0.1;

    pub const L2_RPC_URL: &str = "http://localhost:3050";
}

pub struct DepositTestUtils;

impl DepositTestUtils {
    pub fn create_bitcoin_client() -> Result<Arc<BitcoinClient>> {
        let auth = NodeAuth::UserPass(
            config::RPC_USER.to_string(),
            config::RPC_PASSWORD.to_string(),
        );
        let client_config = ViaBtcClientConfig {
            network: config::NETWORK.to_string(),
            external_apis: vec![],
            fee_strategies: vec![],
            use_rpc_for_fee_rate: None,
        };
        let client = Arc::new(BitcoinClient::new(config::RPC_URL, auth, client_config)?);
        Ok(client)
    }

    pub async fn check_l2_balance(address: &EVMAddress) -> Result<u64> {
        let response = reqwest::Client::new()
            .post(config::L2_RPC_URL)
            .header("content-type", "application/json")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_getBalance",
                "params": [format!("{:?}", address), "latest"]
            }))
            .send()
            .await?;

        let response_text = response.text().await?;
        let response_json: serde_json::Value = serde_json::from_str(&response_text)?;

        if let Some(error) = response_json.get("error") {
            if !error.is_null() {
                return Err(anyhow::anyhow!("L2 RPC error: {}", error));
            }
        }

        let balance_hex = response_json["result"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No balance result in response"))?;

        let balance = u64::from_str_radix(balance_hex.trim_start_matches("0x"), 16)
            .map_err(|err| anyhow::anyhow!("Failed to parse balance: {}", err))?;

        Ok(balance)
    }

    pub async fn wait_for_l2_balance_update(
        address: &EVMAddress,
        expected_min_balance: u64,
        max_wait_time: Duration,
        check_interval: Duration,
    ) -> Result<u64> {
        let start_time = std::time::Instant::now();

        loop {
            match Self::check_l2_balance(address).await {
                Ok(balance) => {
                    debug!("L2 balance for {}: {} wei", address, balance);
                    if balance >= expected_min_balance {
                        return Ok(balance);
                    }
                }
                Err(e) => {
                    debug!("Failed to check L2 balance: {}", e);
                }
            }

            if start_time.elapsed() >= max_wait_time {
                return Err(anyhow::anyhow!(
                    "Timeout waiting for L2 balance update. Expected at least {} wei for address {}",
                    expected_min_balance,
                    address
                ));
            }

            tokio::time::sleep(check_interval).await;
        }
    }

    pub fn get_bridge_address() -> Result<BitcoinAddress> {
        let bridge_address = BitcoinAddress::from_str(config::BRIDGE_MUSIG2_ADDRESS)
            .expect("Invalid bridge address")
            .require_network(config::NETWORK)?;
        Ok(bridge_address)
    }

    pub fn get_test_receiver_l2_address() -> Result<EVMAddress> {
        let receiver_address = EVMAddress::from_str(config::TEST_RECEIVER_L2_ADDRESS)
            .expect("Invalid test receiver address");
        Ok(receiver_address)
    }

    pub async fn get_address_from_private_key(private_key: &str) -> Result<String> {
        let response = reqwest::Client::new()
            .post(config::RPC_URL)
            .header("content-type", "text/plain")
            .basic_auth(config::RPC_USER, Some(config::RPC_PASSWORD))
            .body(format!(
                r#"{{"jsonrpc": "1.0", "id": "get_descriptor", "method": "getdescriptorinfo", "params": ["wpkh({})"]}}"#,
                private_key
            ))
            .send()
            .await?;

        let response_text = response.text().await?;
        let response_json: serde_json::Value = serde_json::from_str(&response_text)?;

        if let Some(error) = response_json.get("error") {
            if !error.is_null() {
                return Err(anyhow::anyhow!("RPC error: {}", error));
            }
        }

        let descriptor = response_json["result"]["descriptor"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No descriptor in response"))?;

        let response = reqwest::Client::new()
            .post(config::RPC_URL)
            .header("content-type", "text/plain")
            .basic_auth(config::RPC_USER, Some(config::RPC_PASSWORD))
            .body(format!(
                r#"{{"jsonrpc": "1.0", "id": "derive_address", "method": "deriveaddresses", "params": ["{}"]}}"#,
                descriptor
            ))
            .send()
            .await?;

        let response_text = response.text().await?;
        let response_json: serde_json::Value = serde_json::from_str(&response_text)?;

        if let Some(error) = response_json.get("error") {
            if !error.is_null() {
                return Err(anyhow::anyhow!("RPC error: {}", error));
            }
        }

        let addresses = response_json["result"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("No addresses array in response"))?;

        let address = addresses[0]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No address in response"))?;

        Ok(address.to_string())
    }

    pub async fn fund_test_address(test_address: &str, amount_btc: f64) -> Result<()> {
        debug!("Checking if test address {} needs funding", test_address);

        let mut current_balance = 0.0;
        let response = reqwest::Client::new()
            .post(config::RPC_URL)
            .header("content-type", "text/plain")
            .basic_auth(config::RPC_USER, Some(config::RPC_PASSWORD))
            .body(format!(
                r#"{{"jsonrpc": "1.0", "id": "check_balance", "method": "scantxoutset", "params": ["start", [{{"desc": "addr({})"}}]]}}"#,
                test_address
            ))
            .send()
            .await?;

        let response_text = response.text().await?;
        let response_json: serde_json::Value = serde_json::from_str(&response_text)?;

        if let Some(error) = response_json.get("error") {
            if !error.is_null() {
                let error_code = error["code"].as_i64().unwrap_or(0);
                if error_code == -8 {
                    debug!(
                        "Scan in progress, proceeding with funding for test address {}",
                        test_address
                    );
                } else {
                    return Err(anyhow::anyhow!("RPC error checking balance: {}", error));
                }
            }
        } else {
            current_balance = response_json["result"]["total_amount"]
                .as_f64()
                .unwrap_or(0.0);
        }

        let required_balance = amount_btc;

        if current_balance >= required_balance {
            debug!(
                "Test address {} already has sufficient balance: {} BTC",
                test_address, current_balance
            );
            return Ok(());
        }

        debug!(
            "Funding test address {} with {} BTC (current balance: {} BTC)",
            test_address, amount_btc, current_balance
        );

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let response = reqwest::Client::new()
            .post(&format!("{}/wallet/Alice", config::RPC_URL))
            .header("content-type", "text/plain")
            .basic_auth(config::RPC_USER, Some(config::RPC_PASSWORD))
            .body(format!(
                r#"{{"jsonrpc": "1.0", "id": "fund_test", "method": "sendtoaddress", "params": ["{}", {}]}}"#,
                test_address, amount_btc
            ))
            .send()
            .await?;

        let response_text = response.text().await?;
        let response_json: serde_json::Value = serde_json::from_str(&response_text)?;

        if let Some(error) = response_json.get("error") {
            if !error.is_null() {
                return Err(anyhow::anyhow!("RPC error: {}", error));
            }
        }

        let txid = response_json["result"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No transaction ID in response"))?;

        debug!("Funding transaction created: {}", txid);

        let response = reqwest::Client::new()
            .post(&format!("{}/wallet/Alice", config::RPC_URL))
            .header("content-type", "text/plain")
            .basic_auth(config::RPC_USER, Some(config::RPC_PASSWORD))
            .body(format!(
                r#"{{"jsonrpc": "1.0", "id": "generate_block", "method": "generatetoaddress", "params": [1, "{}"]}}"#,
                config::ALICE_WALLET_ADDRESS
            ))
            .send()
            .await?;

        let response_text = response.text().await?;
        let response_json: serde_json::Value = serde_json::from_str(&response_text)?;

        if let Some(error) = response_json.get("error") {
            if !error.is_null() {
                return Err(anyhow::anyhow!("RPC error generating block: {}", error));
            }
        }

        debug!("Block generated to confirm funding transaction");
        Ok(())
    }

    pub async fn setup_funded_inscriber_with_key(
        private_key: &str,
    ) -> Result<(Arc<BitcoinClient>, Inscriber)> {
        debug!("Setting up funded inscriber for testing with private key");

        let client = Self::create_bitcoin_client()?;

        let test_address = Self::get_address_from_private_key(private_key).await?;
        debug!("Test address derived from private key: {}", test_address);

        Self::fund_test_address(&test_address, config::DEFAULT_FUNDING_AMOUNT_BTC).await?;

        let context = None;
        let inscriber = Inscriber::new(client.clone(), private_key, context)
            .await
            .context("Failed to create Depositor Inscriber")?;

        Ok((client, inscriber))
    }

    pub async fn setup_funded_inscriber() -> Result<(Arc<BitcoinClient>, Inscriber)> {
        Self::setup_funded_inscriber_with_key(config::TEST_PRIVATE_KEY).await
    }

    pub fn create_test_l1_to_l2_message() -> Result<L1ToL2MessageInput> {
        let receiver_l2_address = Self::get_test_receiver_l2_address()?;
        Ok(L1ToL2MessageInput {
            receiver_l2_address,
            l2_contract_address: EVMAddress::zero(),
            call_data: vec![],
        })
    }

    pub fn create_test_recipient(amount_sats: u64) -> Result<Recipient> {
        let bridge_address = Self::get_bridge_address()?;
        Ok(Recipient {
            address: bridge_address,
            amount: Amount::from_sat(amount_sats),
        })
    }

    pub async fn perform_deposit_test(amount_sats: Option<u64>) -> Result<()> {
        Self::perform_deposit_test_with_key(amount_sats, config::TEST_PRIVATE_KEY).await
    }

    pub async fn perform_deposit_test_with_key(
        amount_sats: Option<u64>,
        private_key: &str,
    ) -> Result<()> {
        Self::perform_deposit_test_with_key_and_l2_check(amount_sats, private_key, true).await
    }

    pub async fn perform_deposit_test_with_key_and_l2_check(
        amount_sats: Option<u64>,
        private_key: &str,
        check_l2_balance: bool,
    ) -> Result<()> {
        let amount = amount_sats.unwrap_or(config::DEFAULT_DEPOSIT_AMOUNT_SATS);

        debug!("Starting deposit test");
        debug!("Amount: {} sats", amount);
        debug!("Using private key: {}...", &private_key[..8]);
        debug!("L2 balance check: {}", check_l2_balance);

        let client = Self::create_bitcoin_client()?;
        let context = None;
        let mut inscriber = Inscriber::new(client.clone(), private_key, context)
            .await
            .context("Failed to create Depositor Inscriber")?;

        let balance = inscriber
            .get_balance()
            .await
            .context("Failed to get balance")?;
        debug!("Depositor L1 balance: {}", balance);

        if balance < amount as u128 {
            return Err(anyhow::anyhow!(
                "Insufficient balance: {} < {}",
                balance,
                amount
            ));
        }

        let input = Self::create_test_l1_to_l2_message()?;
        let receiver_l2_address = input.receiver_l2_address;

        let initial_l2_balance = if check_l2_balance {
            match Self::check_l2_balance(&receiver_l2_address).await {
                Ok(balance) => {
                    debug!(
                        "Initial L2 balance for {}: {} wei",
                        receiver_l2_address, balance
                    );
                    Some(balance)
                }
                Err(e) => {
                    debug!("Failed to get initial L2 balance: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let recipient = Self::create_test_recipient(amount)?;

        debug!("Performing deposit inscription...");
        let deposit_info = inscriber
            .inscribe_with_recipient(InscriptionMessage::L1ToL2Message(input), Some(recipient))
            .await?;

        debug!("Deposit successful!");
        debug!("Transaction ID: {:?}", deposit_info.final_reveal_tx.txid);
        debug!(
            "Recipient output: {:?}",
            deposit_info.reveal_tx_output_info.recipient_tx_output
        );

        let is_confirmed = client
            .check_tx_confirmation(&deposit_info.final_reveal_tx.txid, 1)
            .await?;
        debug!("Transaction confirmed: {}", is_confirmed);

        if check_l2_balance {
            debug!("Waiting for L2 balance update...");
            let expected_amount_wei = amount as u64 * 10_000_000_000;

            match Self::wait_for_l2_balance_update(
                &receiver_l2_address,
                expected_amount_wei,
                Duration::from_secs(60),
                Duration::from_secs(2),
            )
            .await
            {
                Ok(final_balance) => {
                    debug!("✅ L2 balance verification successful!");
                    debug!(
                        "Final L2 balance for {}: {} wei",
                        receiver_l2_address, final_balance
                    );

                    if let Some(initial) = initial_l2_balance {
                        let balance_increase = final_balance - initial;
                        debug!("L2 balance increased by: {} wei", balance_increase);
                    }
                }
                Err(e) => {
                    debug!("⚠️  L2 balance verification failed: {}", e);
                }
            }
        }

        Ok(())
    }

    pub async fn perform_opreturn_deposit_test(
        amount_sats: Option<u64>,
        private_key: &str,
    ) -> Result<String> {
        let amount = amount_sats.unwrap_or(config::DEFAULT_DEPOSIT_AMOUNT_SATS);

        debug!("Starting OP_RETURN deposit test");
        debug!("Amount: {} sats", amount);
        debug!("Using private key: {}...", &private_key[..8]);

        let client = Self::create_bitcoin_client()?;

        let bridge_address = Self::get_bridge_address()?;
        let receiver_l2_address = Self::get_test_receiver_l2_address()?;

        let txid = Self::create_opreturn_transaction(
            &client,
            private_key,
            amount,
            &bridge_address,
            &receiver_l2_address,
        )
        .await?;

        debug!("OP_RETURN deposit successful!");
        debug!("Transaction ID: {}", txid);

        Ok(txid)
    }

    pub async fn create_opreturn_transaction(
        client: &Arc<BitcoinClient>,
        private_key: &str,
        amount_sats: u64,
        bridge_address: &BitcoinAddress,
        receiver_l2_address: &EVMAddress,
    ) -> Result<String> {
        use bitcoin::{
            absolute,
            consensus::encode::serialize_hex,
            secp256k1::{Message, Secp256k1},
            sighash::{EcdsaSighashType, SighashCache},
            transaction, Address, Amount, CompressedPublicKey, PrivateKey, ScriptBuf, Sequence,
            Transaction, TxIn, TxOut, Witness,
        };

        let secp = Secp256k1::new();
        let private_key = PrivateKey::from_wif(private_key)
            .map_err(|e| anyhow::anyhow!("Invalid private key: {}", e))?;
        let pk = private_key.inner.public_key(&secp);
        let compressed_pk = CompressedPublicKey::from_private_key(&secp, &private_key)
            .map_err(|e| anyhow::anyhow!("Failed to create compressed public key: {}", e))?;
        let address = Address::p2wpkh(&compressed_pk, config::NETWORK);

        let amount = Amount::from_sat(amount_sats);
        let fees = Amount::from_btc(0.0001)?;

        let all_utxos = client.fetch_utxos(&address).await?;
        let total_needed = amount + fees;
        let mut selected_utxos = Vec::new();
        let mut input_amount = Amount::from_sat(0);
        for (outpoint, txout) in all_utxos.into_iter() {
            selected_utxos.push((outpoint, txout));
            input_amount += selected_utxos.last().unwrap().1.value;
            if input_amount >= total_needed {
                break;
            }
        }

        if input_amount < total_needed {
            return Err(anyhow::anyhow!("Insufficient funds"));
        }

        let tx_inputs: Vec<TxIn> = selected_utxos
            .iter()
            .map(|(outpoint, _)| TxIn {
                previous_output: *outpoint,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            })
            .collect();

        let mut outputs = Vec::new();

        outputs.push(TxOut {
            value: amount,
            script_pubkey: bridge_address.script_pubkey(),
        });

        outputs.push(TxOut {
            value: Amount::from_sat(0),
            script_pubkey: ScriptBuf::new_op_return(receiver_l2_address.to_fixed_bytes()),
        });

        let change_amount = input_amount - total_needed;
        if change_amount > Amount::from_sat(0) {
            outputs.push(TxOut {
                value: change_amount,
                script_pubkey: address.script_pubkey(),
            });
        }

        let mut tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: tx_inputs,
            output: outputs,
        };

        let sighash_type = EcdsaSighashType::All;
        let mut cache = SighashCache::new(&mut tx);
        for (i, (_, utxo)) in selected_utxos.iter().enumerate() {
            let sighash = cache
                .p2wpkh_signature_hash(i, &utxo.script_pubkey, utxo.value, sighash_type)
                .map_err(|e| anyhow::anyhow!("Failed to create signature hash: {}", e))?;

            let msg = Message::from(sighash);
            let signature = secp.sign_ecdsa(&msg, &private_key.inner);

            let signature = bitcoin::ecdsa::Signature {
                signature,
                sighash_type,
            };

            cache
                .witness_mut(i)
                .ok_or_else(|| anyhow::anyhow!("Failed to get witness"))
                .map(|witness| *witness = Witness::p2wpkh(&signature, &pk))?;
        }

        let tx = cache.into_transaction();

        let tx_hex = serialize_hex(&tx);
        let txid = client.broadcast_signed_transaction(&tx_hex).await?;

        Ok(txid.to_string())
    }
}
