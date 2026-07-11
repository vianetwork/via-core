use std::{
    env,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{bail, ensure, Context, Result};
use bitcoin::{
    absolute,
    consensus::encode::serialize_hex,
    hashes::{sha256, Hash},
    secp256k1::{Keypair, Message, Secp256k1, SecretKey, XOnlyPublicKey},
    sighash::{EcdsaSighashType, SighashCache},
    transaction, Address, Amount, CompressedPublicKey, Network, PrivateKey, ScriptBuf, Sequence, Transaction, TxIn,
    TxOut, Txid, Witness,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use via_btc_client::{
    client::BitcoinClient,
    inscriber::Inscriber,
    traits::BitcoinOps,
    types::{
        InscriptionMessage, L1BatchDAReferenceInput, L1ToL2MessageInput, NodeAuth, ProofDAReferenceInput, Recipient,
        SystemBootstrappingInput, ValidatorAttestationInput, Vote,
    },
};
use via_btc_ingestion::{ProtocolContext, ProtocolVersionTag, Role, WalletSet};
use via_ingestion_shadow::{execute, Mode, RunConfig};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::{
    protocol_version::{ProtocolSemanticVersion, ProtocolVersionId, VersionPatch},
    Address as EvmAddress, L1BatchNumber, H256,
};

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

fn split_database_url(url: &str) -> (&str, &str) {
    url.rsplit_once('/').expect("TEST_DATABASE_URL must contain a database name")
}

async fn admin_pool(base: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .connect(&format!("{base}/postgres"))
        .await
        .expect("connect to Postgres admin database")
}

async fn clone_dev_test() -> (String, PgPool, String) {
    let template_url = env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must provide compose credentials for the dev_test template");
    let (base, _) = split_database_url(&template_url);
    let database = format!("via_shadow_e2e_{}_{}", std::process::id(), DB_COUNTER.fetch_add(1, Ordering::SeqCst));
    let admin = admin_pool(base).await;
    for attempt in 0..20 {
        let result = sqlx::query(&format!("CREATE DATABASE \"{database}\" TEMPLATE dev_test")).execute(&admin).await;
        match result {
            Ok(_) => break,
            Err(err)
                if attempt < 19
                    && err.as_database_error().and_then(|error| error.code()).is_some_and(|code| code == "55006") =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            Err(err) => panic!("clone dev_test template: {err}"),
        }
    }
    (format!("{base}/{database}"), admin, database)
}

async fn drop_database(admin: &PgPool, database: &str) {
    sqlx::query(&format!("DROP DATABASE \"{database}\" WITH (FORCE)"))
        .execute(admin)
        .await
        .expect("drop throwaway shadow database");
}

fn empty_context() -> ProtocolContext {
    ProtocolContext {
        version: 1,
        wallets: WalletSet {
            sequencer: ScriptBuf::new(),
            bridge: ScriptBuf::new(),
            governance: ScriptBuf::new(),
            verifiers: vec![],
        },
        protocol_version: ProtocolVersionTag { minor: 0, patch: 0 },
    }
}

#[derive(Clone)]
struct NodeRpc {
    client: reqwest::Client,
    root_url: String,
    wallet_url: String,
    user: String,
    password: String,
    wallet_name: String,
}

impl NodeRpc {
    async fn connect(root_url: &str, user: &str, password: &str) -> Result<Self> {
        let root_url = root_url.trim_end_matches('/').to_owned();
        let mut rpc = Self {
            client: reqwest::Client::new(),
            root_url: root_url.clone(),
            wallet_url: root_url.clone(),
            user: user.to_owned(),
            password: password.to_owned(),
            wallet_name: String::new(),
        };
        let wallets = rpc
            .call(&root_url, "listwallets", json!([]))
            .await?
            .as_array()
            .context("listwallets result is not an array")?
            .iter()
            .map(|wallet| wallet.as_str().context("loaded wallet name is not a string").map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
        let preferred = env::var("VIA_SHADOW_E2E_WALLET").unwrap_or_else(|_| "shadow".to_owned());
        let wallet_name = wallets
            .iter()
            .find(|wallet| **wallet == preferred)
            .or_else(|| wallets.first())
            .cloned()
            .context("the regtest node has no loaded wallet")?;
        rpc.wallet_url = format!("{root_url}/wallet/{wallet_name}");
        rpc.wallet_name = wallet_name;
        Ok(rpc)
    }

    async fn call(&self, url: &str, method: &str, params: Value) -> Result<Value> {
        let payload = json!({"jsonrpc": "1.0", "id": "via-shadow-e2e", "method": method, "params": params});
        let body = self
            .client
            .post(url)
            .header("content-type", "text/plain")
            .basic_auth(&self.user, Some(&self.password))
            .body(payload.to_string())
            .send()
            .await
            .with_context(|| format!("call Bitcoin RPC method {method}"))?
            .error_for_status()
            .with_context(|| format!("Bitcoin RPC method {method} returned an HTTP error"))?
            .text()
            .await
            .with_context(|| format!("read Bitcoin RPC method {method} response"))?;
        let response: Value =
            serde_json::from_str(&body).with_context(|| format!("decode Bitcoin RPC method {method} response"))?;
        if response.get("error").is_some_and(|error| !error.is_null()) {
            bail!("Bitcoin RPC method {method} failed: {}", response["error"]);
        }
        response.get("result").cloned().with_context(|| format!("Bitcoin RPC method {method} omitted result"))
    }

    async fn block_count(&self) -> Result<u64> {
        self.call(&self.root_url, "getblockcount", json!([]))
            .await?
            .as_u64()
            .context("getblockcount result is not an unsigned integer")
    }

    async fn new_mining_address(&self) -> Result<Address> {
        let raw = self
            .call(&self.wallet_url, "getnewaddress", json!(["via-shadow-e2e", "bech32"]))
            .await?
            .as_str()
            .context("getnewaddress result is not a string")?
            .to_owned();
        raw.parse::<Address<_>>()?.require_network(Network::Regtest).context("wallet returned a non-regtest address")
    }

    async fn send_to_address(&self, address: &Address, amount: Amount) -> Result<Txid> {
        let raw = self
            .call(&self.wallet_url, "sendtoaddress", json!([address.to_string(), amount.to_btc()]))
            .await?
            .as_str()
            .context("sendtoaddress result is not a txid")?
            .to_owned();
        raw.parse().context("parse sendtoaddress txid")
    }

    async fn mine(&self, blocks: u64, address: &Address) -> Result<u64> {
        self.call(&self.wallet_url, "generatetoaddress", json!([blocks, address.to_string()])).await?;
        self.block_count().await
    }
}

struct FeeServer {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
    url: String,
}

impl FeeServer {
    fn start() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("bind deterministic fee fixture")?;
        listener.set_nonblocking(true).context("make deterministic fee fixture nonblocking")?;
        let address = listener.local_addr().context("read deterministic fee fixture address")?;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = serve_fee_response(stream);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self { stop, thread: Some(thread), url: format!("http://{address}/fee") })
    }
}

impl Drop for FeeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_fee_response(mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = [0_u8; 1024];
    let _ = stream.read(&mut request)?;
    let body = r#"{"fastestFee":1}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}

fn seeded_private_key(height: u64, label: &str) -> PrivateKey {
    for nonce in 0_u32.. {
        let mut seed = b"via-shadow-e2e-v1".to_vec();
        seed.extend_from_slice(&height.to_be_bytes());
        seed.extend_from_slice(label.as_bytes());
        seed.extend_from_slice(&nonce.to_be_bytes());
        let bytes = sha256::Hash::hash(&seed).to_byte_array();
        if let Ok(secret) = SecretKey::from_slice(&bytes) {
            return PrivateKey::new(secret, Network::Regtest);
        }
    }
    unreachable!("the finite secp256k1 key space has valid keys")
}

fn p2wpkh_address(private_key: &PrivateKey) -> Result<Address> {
    let secp = Secp256k1::new();
    let key = CompressedPublicKey::from_private_key(&secp, private_key).context("derive compressed public key")?;
    Ok(Address::p2wpkh(&key, Network::Regtest))
}

fn p2tr_address(private_key: &PrivateKey) -> Address {
    let secp = Secp256k1::new();
    let keypair = Keypair::from_secret_key(&secp, &private_key.inner);
    let (xonly, _) = XOnlyPublicKey::from_keypair(&keypair);
    Address::p2tr(&secp, xonly, None, Network::Regtest)
}

struct Scenario {
    initial_tip: u64,
    funding_height: u64,
    funding_strategy: &'static str,
    funding_txids: Vec<Txid>,
    from_height: u64,
    to_height: u64,
    bootstrap_height: u64,
    bootstrap_txid: Txid,
    deposits_height: u64,
    op_return_deposit_txid: Txid,
    inscription_deposit_txid: Txid,
    op_return_amount: u64,
    inscription_amount: u64,
    op_return_receiver: [u8; 20],
    inscription_receiver: [u8; 20],
    batch_height: u64,
    batch_txid: Txid,
    proof_height: u64,
    proof_txid: Txid,
    attestation_height: u64,
    attestation_txid: Txid,
}

async fn fund_scenario_signers(
    rpc: &NodeRpc, addresses: &[Address], miner: &Address,
) -> Result<(u64, &'static str, Vec<Txid>)> {
    let mut txids = Vec::with_capacity(addresses.len());
    let mut wallet_error = None;
    for address in addresses {
        match rpc.send_to_address(address, Amount::from_sat(5_000_000)).await {
            Ok(txid) => txids.push(txid),
            Err(err) => {
                wallet_error = Some(err);
                break;
            }
        }
    }
    if wallet_error.is_none() {
        return Ok((rpc.mine(1, miner).await?, "wallet-sendtoaddress", txids));
    }

    println!("wallet funding unavailable; using generatetoaddress maturity fallback: {}", wallet_error.unwrap());
    for address in addresses {
        rpc.mine(1, address).await?;
    }
    let height = rpc.mine(100, miner).await?;
    Ok((height, "generatetoaddress-maturity-fallback", txids))
}

async fn mine_transactions(rpc: &NodeRpc, client: &BitcoinClient, miner: &Address, expected: &[Txid]) -> Result<u64> {
    let height = rpc.mine(1, miner).await?;
    let block =
        client.fetch_block(u128::from(height)).await.with_context(|| format!("fetch newly mined block {height}"))?;
    for expected_txid in expected {
        ensure!(
            block.txdata.iter().any(|transaction| transaction.compute_txid() == *expected_txid),
            "newly mined block {height} does not contain expected txid {expected_txid}"
        );
    }
    Ok(height)
}

async fn broadcast_op_return_deposit(
    client: &BitcoinClient, private_key: &PrivateKey, bridge: &Address, receiver: [u8; 20], amount_sat: u64,
) -> Result<Txid> {
    let secp = Secp256k1::new();
    let public_key = private_key.inner.public_key(&secp);
    let address = p2wpkh_address(private_key)?;
    let all_utxos = client.fetch_utxos(&address).await.context("fetch OP_RETURN depositor UTXOs")?;
    let amount = Amount::from_sat(amount_sat);
    let fee = Amount::from_sat(10_000);
    let total_needed = amount + fee;
    let mut selected = Vec::new();
    let mut input_amount = Amount::ZERO;
    for utxo in all_utxos {
        input_amount += utxo.1.value;
        selected.push(utxo);
        if input_amount >= total_needed {
            break;
        }
    }
    ensure!(input_amount >= total_needed, "fresh OP_RETURN depositor has insufficient confirmed funds");

    let inputs = selected
        .iter()
        .map(|(outpoint, _)| TxIn {
            previous_output: *outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::new(),
        })
        .collect();
    let mut outputs = vec![
        TxOut { value: amount, script_pubkey: bridge.script_pubkey() },
        TxOut { value: Amount::ZERO, script_pubkey: ScriptBuf::new_op_return(receiver) },
    ];
    let change = input_amount - total_needed;
    if change > Amount::ZERO {
        outputs.push(TxOut { value: change, script_pubkey: address.script_pubkey() });
    }
    let mut transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: inputs,
        output: outputs,
    };
    let sighash_type = EcdsaSighashType::All;
    let mut cache = SighashCache::new(&mut transaction);
    for (index, (_, utxo)) in selected.iter().enumerate() {
        let sighash = cache
            .p2wpkh_signature_hash(index, &utxo.script_pubkey, utxo.value, sighash_type)
            .context("create OP_RETURN deposit sighash")?;
        let message = Message::from_digest(sighash.to_byte_array());
        let signature =
            bitcoin::ecdsa::Signature { signature: secp.sign_ecdsa(&message, &private_key.inner), sighash_type };
        *cache.witness_mut(index).context("locate OP_RETURN deposit witness")? =
            Witness::p2wpkh(&signature, &public_key);
    }
    let transaction = cache.into_transaction();
    let expected = transaction.compute_txid();
    let broadcast = client
        .broadcast_signed_transaction(&serialize_hex(transaction))
        .await
        .context("broadcast OP_RETURN deposit")?;
    ensure!(broadcast == expected, "broadcast RPC returned a byte-order-inconsistent OP_RETURN txid");
    Ok(broadcast)
}

async fn build_scenario(rpc: &NodeRpc, client: Arc<BitcoinClient>) -> Result<Scenario> {
    let initial_tip = rpc.block_count().await?;
    let sequencer_key = seeded_private_key(initial_tip, "sequencer");
    let governance_key = seeded_private_key(initial_tip, "governance");
    let verifier_key = seeded_private_key(initial_tip, "verifier");
    let bridge_key = seeded_private_key(initial_tip, "bridge");
    let op_return_depositor_key = seeded_private_key(initial_tip, "op-return-depositor");
    let inscription_depositor_key = seeded_private_key(initial_tip, "inscription-depositor");
    let sequencer = p2wpkh_address(&sequencer_key)?;
    let governance = p2wpkh_address(&governance_key)?;
    let verifier = p2wpkh_address(&verifier_key)?;
    let bridge = p2tr_address(&bridge_key);
    let op_return_depositor = p2wpkh_address(&op_return_depositor_key)?;
    let inscription_depositor = p2wpkh_address(&inscription_depositor_key)?;
    let miner = rpc.new_mining_address().await?;
    let (funding_height, funding_strategy, funding_txids) = fund_scenario_signers(
        rpc,
        &[sequencer.clone(), verifier.clone(), op_return_depositor, inscription_depositor],
        &miner,
    )
    .await?;
    let from_height = funding_height.checked_add(1).context("funding height overflow")?;

    let mut sequencer_inscriber = Inscriber::new(client.clone(), &sequencer_key.to_wif(), None)
        .await
        .context("construct fresh sequencer Inscriber")?;
    let bootstrap = SystemBootstrappingInput {
        start_block_height: u32::try_from(from_height).context("bootstrap height exceeds u32")?,
        protocol_version: ProtocolSemanticVersion::new(ProtocolVersionId::Version26, VersionPatch(0)),
        bootloader_hash: H256::from([1_u8; 32]),
        abstract_account_hash: H256::from([2_u8; 32]),
        snark_wrapper_vk_hash: H256::from([3_u8; 32]),
        evm_emulator_hash: H256::from([4_u8; 32]),
        governance_address: governance.as_unchecked().clone(),
        sequencer_address: sequencer.as_unchecked().clone(),
        bridge_musig2_address: bridge.as_unchecked().clone(),
        verifier_p2wpkh_addresses: vec![verifier.as_unchecked().clone()],
    };
    let bootstrap_info = sequencer_inscriber
        .inscribe(InscriptionMessage::SystemBootstrapping(bootstrap))
        .await
        .context("broadcast SystemBootstrapping inscription")?;
    let bootstrap_txid = bootstrap_info.final_reveal_tx.txid;
    let bootstrap_height = mine_transactions(rpc, &client, &miner, &[bootstrap_txid]).await?;
    ensure!(bootstrap_height == from_height, "bootstrap did not land at the replay range start");

    let op_return_receiver = [0x11_u8; 20];
    let inscription_receiver = [0x22_u8; 20];
    let op_return_amount = 111_111;
    let inscription_amount = 222_222;
    let op_return_deposit_txid =
        broadcast_op_return_deposit(&client, &op_return_depositor_key, &bridge, op_return_receiver, op_return_amount)
            .await?;
    let mut depositor_inscriber = Inscriber::new(client.clone(), &inscription_depositor_key.to_wif(), None)
        .await
        .context("construct fresh inscription depositor")?;
    let inscription_info = depositor_inscriber
        .inscribe_with_recipient(
            InscriptionMessage::L1ToL2Message(L1ToL2MessageInput {
                receiver_l2_address: EvmAddress::from(inscription_receiver),
                l2_contract_address: EvmAddress::zero(),
                call_data: vec![],
            }),
            Some(Recipient { address: bridge.clone(), amount: Amount::from_sat(inscription_amount) }),
        )
        .await
        .context("broadcast inscription deposit")?;
    let inscription_deposit_txid = inscription_info.final_reveal_tx.txid;
    let deposits_height =
        mine_transactions(rpc, &client, &miner, &[op_return_deposit_txid, inscription_deposit_txid]).await?;

    sequencer_inscriber.sync_context_with_blockchain().await.context("confirm bootstrap Inscriber context")?;
    let batch_info = sequencer_inscriber
        .inscribe(InscriptionMessage::L1BatchDAReference(L1BatchDAReferenceInput {
            l1_batch_hash: H256::from([5_u8; 32]),
            l1_batch_index: L1BatchNumber(1),
            da_identifier: "via-shadow-e2e-da".to_owned(),
            blob_id: "via-shadow-e2e-batch".to_owned(),
            prev_l1_batch_hash: H256::zero(),
        }))
        .await
        .context("broadcast L1 batch reference")?;
    let batch_txid = batch_info.final_reveal_tx.txid;
    let batch_height = mine_transactions(rpc, &client, &miner, &[batch_txid]).await?;

    sequencer_inscriber.sync_context_with_blockchain().await.context("confirm batch Inscriber context")?;
    let proof_info = sequencer_inscriber
        .inscribe(InscriptionMessage::ProofDAReference(ProofDAReferenceInput {
            l1_batch_reveal_txid: batch_txid,
            da_identifier: "via-shadow-e2e-da".to_owned(),
            blob_id: "via-shadow-e2e-proof".to_owned(),
        }))
        .await
        .context("broadcast proof reference")?;
    let proof_txid = proof_info.final_reveal_tx.txid;
    let proof_height = mine_transactions(rpc, &client, &miner, &[proof_txid]).await?;

    let mut verifier_inscriber = Inscriber::new(client.clone(), &verifier_key.to_wif(), None)
        .await
        .context("construct fresh verifier Inscriber")?;
    let attestation_info = verifier_inscriber
        .inscribe(InscriptionMessage::ValidatorAttestation(ValidatorAttestationInput {
            reference_txid: proof_txid,
            attestation: Vote::Ok,
        }))
        .await
        .context("broadcast validator attestation")?;
    let attestation_txid = attestation_info.final_reveal_tx.txid;
    let attestation_height = mine_transactions(rpc, &client, &miner, &[attestation_txid]).await?;
    let to_height = rpc.mine(2, &miner).await?;

    Ok(Scenario {
        initial_tip,
        funding_height,
        funding_strategy,
        funding_txids,
        from_height,
        to_height,
        bootstrap_height,
        bootstrap_txid,
        deposits_height,
        op_return_deposit_txid,
        inscription_deposit_txid,
        op_return_amount,
        inscription_amount,
        op_return_receiver,
        inscription_receiver,
        batch_height,
        batch_txid,
        proof_height,
        proof_txid,
        attestation_height,
        attestation_txid,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_coinbase_only_range_and_audit_coverage() {
    let Ok(rpc_url) = env::var("VIA_SHADOW_E2E_RPC_URL") else {
        println!("skipped: VIA_SHADOW_E2E_RPC_URL is unset");
        return;
    };
    let rpc_user =
        env::var("VIA_BTC_CLIENT_RPC_USER").expect("VIA_BTC_CLIENT_RPC_USER must be set for the shadow E2E test");
    let rpc_password = env::var("VIA_BTC_CLIENT_RPC_PASSWORD")
        .expect("VIA_BTC_CLIENT_RPC_PASSWORD must be set for the shadow E2E test");
    let client = Arc::new(
        BitcoinClient::new(&rpc_url, NodeAuth::UserPass(rpc_user, rpc_password), ViaBtcClientConfig::for_tests())
            .expect("construct regtest Bitcoin client"),
    );
    for height in 1..=3 {
        let block = match tokio::time::timeout(std::time::Duration::from_secs(30), client.fetch_block(height)).await {
            Ok(Ok(block)) => block,
            Ok(Err(err)) => {
                println!("skipped: local regtest node is unreachable: {err}");
                return;
            }
            Err(_) => {
                println!("skipped: timed out waiting for local regtest node");
                return;
            }
        };
        assert_eq!(block.txdata.len(), 1, "height {height} is not coinbase-only");
        assert!(block.txdata[0].is_coinbase(), "height {height} does not contain a coinbase transaction");
    }

    let (database_url, admin, database) = clone_dev_test().await;
    let mut replay_task = tokio::spawn(execute(RunConfig {
        client,
        network: Network::Regtest,
        database_url,
        role: Role::CoreSequencer,
        from_height: 1,
        to_height: 3,
        mode: Mode::Replay,
        out: None,
        bootstrap_context: Some(empty_context()),
    }));
    let execution = match tokio::time::timeout(std::time::Duration::from_secs(120), &mut replay_task).await {
        Ok(execution) => Some(execution),
        Err(_) => {
            replay_task.abort();
            let _ = replay_task.await;
            None
        }
    };
    drop_database(&admin, &database).await;
    admin.close().await;

    let execution = execution.unwrap_or_else(|| panic!("shadow replay timed out"));
    let result = match execution {
        Ok(result) => result,
        Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
        Err(err) => panic!("replay task was cancelled: {err}"),
    };
    let summary = result.expect("replay coinbase-only range");
    assert_eq!(summary.blocks_applied, 3);
    assert!(summary.events_per_kind.is_empty());
    assert!(summary.rejections_per_code.is_empty());
    assert!(summary.coverage.contiguous);
    assert!(summary.coverage.unresolved_dependencies.is_empty());
}

async fn replay_lifecycle(client: Arc<BitcoinClient>, scenario: &Scenario) -> Result<Value> {
    let (database_url, admin, database) = clone_dev_test().await;
    let outcome = async {
        let summary = execute(RunConfig {
            client,
            network: Network::Regtest,
            database_url: database_url.clone(),
            role: Role::CoreSequencer,
            from_height: scenario.from_height,
            to_height: scenario.to_height,
            mode: Mode::Replay,
            out: None,
            bootstrap_context: Some(empty_context()),
        })
        .await
        .context("replay full lifecycle range")?;
        ensure!(summary.blocks_applied == scenario.to_height - scenario.from_height + 1);
        ensure!(summary.events_per_kind.get("system_bootstrapping") == Some(&1));
        ensure!(summary.events_per_kind.get("deposit") == Some(&2));
        ensure!(summary.events_per_kind.get("l1_batch_da_reference") == Some(&1));
        ensure!(summary.events_per_kind.get("proof_da_reference") == Some(&1));
        ensure!(summary.events_per_kind.get("validator_attestation") == Some(&1));
        ensure!(summary.coverage.contiguous);
        ensure!(summary.coverage.unresolved_dependencies.is_empty());

        let pool = PgPoolOptions::new().max_connections(2).connect(&database_url).await?;
        let (context_blob,): (Value,) =
            sqlx::query_as("SELECT context_blob FROM via_ingestion_checkpoint WHERE id")
                .fetch_one(&pool)
                .await
                .context("load replay checkpoint context")?;
        let context: ProtocolContext = serde_json::from_value(context_blob).context("decode replay checkpoint context")?;
        ensure!(context.wallets.is_bootstrapped(), "replay checkpoint remained unbootstrapped");
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT amount_sat, receiver FROM via_ingestion_deposits WHERE height BETWEEN $1 AND $2 ORDER BY amount_sat",
        )
        .bind(i64::try_from(scenario.from_height)?)
        .bind(i64::try_from(scenario.to_height)?)
        .fetch_all(&pool)
        .await
        .context("load replay deposit projections")?;
        pool.close().await;
        let expected = vec![
            (i64::try_from(scenario.op_return_amount)?, scenario.op_return_receiver.to_vec()),
            (i64::try_from(scenario.inscription_amount)?, scenario.inscription_receiver.to_vec()),
        ];
        ensure!(rows == expected, "replay deposit amounts or receivers differ: {rows:?}");
        serde_json::to_value(summary).context("serialize replay summary")
    }
    .await;
    drop_database(&admin, &database).await;
    admin.close().await;
    outcome
}

async fn shadow_lifecycle(client: Arc<BitcoinClient>, scenario: &Scenario) -> Result<(Value, Vec<Value>, String)> {
    let directory = tempfile::tempdir().context("create shadow delta temp directory")?;
    let output_path = directory.path().join("deltas.jsonl");
    let (database_url, admin, database) = clone_dev_test().await;
    let execution = execute(RunConfig {
        client,
        network: Network::Regtest,
        database_url,
        role: Role::CoreSequencer,
        from_height: scenario.from_height,
        to_height: scenario.to_height,
        mode: Mode::Shadow,
        out: Some(output_path.clone()),
        bootstrap_context: Some(empty_context()),
    })
    .await;
    drop_database(&admin, &database).await;
    admin.close().await;
    let summary = execution.context("shadow full lifecycle range")?;
    ensure!(summary.blocks_applied == scenario.to_height - scenario.from_height + 1);
    ensure!(summary.coverage.contiguous);
    ensure!(summary.coverage.unresolved_dependencies.is_empty());
    let jsonl = std::fs::read_to_string(&output_path).context("read shadow delta JSONL")?;
    let records = jsonl
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).context("decode shadow delta JSONL record"))
        .collect::<Result<Vec<_>>>()?;
    let deltas = records.iter().filter(|record| record.get("side").is_some()).cloned().collect::<Vec<_>>();
    ensure!(
        deltas.len() as u64 == summary.kernel_only + summary.legacy_only + summary.context_differences,
        "shadow summary delta count does not match its JSONL records"
    );
    Ok((serde_json::to_value(summary)?, deltas, jsonl))
}

#[tokio::test(flavor = "multi_thread")]
async fn real_lifecycle_replays_and_shadows_from_unbootstrapped_context() -> Result<()> {
    let Ok(rpc_url) = env::var("VIA_SHADOW_E2E_RPC_URL") else {
        println!("skipped: VIA_SHADOW_E2E_RPC_URL is unset");
        return Ok(());
    };
    let rpc_user =
        env::var("VIA_BTC_CLIENT_RPC_USER").context("VIA_BTC_CLIENT_RPC_USER must be set for the shadow E2E test")?;
    let rpc_password = env::var("VIA_BTC_CLIENT_RPC_PASSWORD")
        .context("VIA_BTC_CLIENT_RPC_PASSWORD must be set for the shadow E2E test")?;
    let fee_server = FeeServer::start()?;
    let client = Arc::new(
        BitcoinClient::new(
            &rpc_url,
            NodeAuth::UserPass(rpc_user.clone(), rpc_password.clone()),
            ViaBtcClientConfig {
                network: Network::Regtest.to_string(),
                external_apis: vec![fee_server.url.clone()],
                fee_strategies: vec!["fastestFee".to_owned()],
                use_rpc_for_fee_rate: Some(false),
            },
        )
        .context("construct lifecycle regtest Bitcoin client")?,
    );
    let rpc = NodeRpc::connect(&rpc_url, &rpc_user, &rpc_password).await?;
    let scenario = build_scenario(&rpc, client.clone()).await?;
    let replay_summary = replay_lifecycle(client.clone(), &scenario).await?;
    let (shadow_summary, shadow_deltas, shadow_delta_jsonl) = shadow_lifecycle(client, &scenario).await?;

    // The scenario builds exactly these protocol effects; the kernel must
    // apply all of them, reject nothing, and cover the range.
    let events = &replay_summary["events_per_kind"];
    assert_eq!(events["deposit"], 2);
    assert_eq!(events["system_bootstrapping"], 1);
    assert_eq!(events["l1_batch_da_reference"], 1);
    assert_eq!(events["proof_da_reference"], 1);
    assert_eq!(events["validator_attestation"], 1);
    assert_eq!(
        replay_summary["rejections_per_code"].as_object().map(|m| m.len()),
        Some(0),
        "no rejections expected: {}",
        replay_summary["rejections_per_code"]
    );
    assert_eq!(replay_summary["coverage"]["contiguous"], true);
    assert_eq!(replay_summary["coverage"]["unresolved_dependencies"].as_array().map(|a| a.len()), Some(0));

    // The load-bearing shadow claim, now enforced: full-fidelity comparison
    // against the legacy path over a real lifecycle yields no disagreement.
    assert_eq!(shadow_summary["kernel_only"], 0, "kernel-only deltas: {shadow_deltas:?}");
    assert_eq!(shadow_summary["legacy_only"], 0, "legacy-only deltas: {shadow_deltas:?}");
    assert_eq!(shadow_summary["context_differences"], 0, "context-transition deltas: {shadow_deltas:?}");
    assert!(shadow_deltas.is_empty(), "expected zero deltas, got {shadow_deltas:?}");

    println!(
        "E2E_SCENARIO_RESULT={}",
        json!({
            "wallet": rpc.wallet_name,
            "initial_tip": scenario.initial_tip,
            "funding": {
                "height": scenario.funding_height,
                "strategy": scenario.funding_strategy,
                "txids": scenario.funding_txids.iter().map(ToString::to_string).collect::<Vec<_>>(),
            },
            "range": {"from": scenario.from_height, "to": scenario.to_height},
            "bootstrap": {"height": scenario.bootstrap_height, "txid": scenario.bootstrap_txid.to_string()},
            "deposits": {
                "height": scenario.deposits_height,
                "op_return": {
                    "txid": scenario.op_return_deposit_txid.to_string(),
                    "amount_sat": scenario.op_return_amount,
                    "receiver": scenario.op_return_receiver,
                },
                "inscription": {
                    "txid": scenario.inscription_deposit_txid.to_string(),
                    "amount_sat": scenario.inscription_amount,
                    "receiver": scenario.inscription_receiver,
                },
            },
            "validator_chain": {
                "batch": {"height": scenario.batch_height, "txid": scenario.batch_txid.to_string()},
                "proof": {"height": scenario.proof_height, "txid": scenario.proof_txid.to_string()},
                "attestation": {"height": scenario.attestation_height, "txid": scenario.attestation_txid.to_string()},
            },
            "replay_summary": replay_summary,
            "shadow_summary": shadow_summary,
            "shadow_deltas": shadow_deltas,
            "shadow_delta_jsonl_verbatim": shadow_delta_jsonl,
        })
    );
    Ok(())
}
