//! Outcomes of `verify_batch_proof` for genuine v27 and v28 proof packages, and of `record_approval` in a database.

use std::future::Future;

use via_btc_client::types::L1BatchDAReferenceInput;
use via_da_client::types::L1MessengerL2ToL1Log;
use via_verification::{
    version_27::types::ProveBatches as ProveBatchesV27,
    version_28::types::ProveBatches as ProveBatchesV28,
};
use via_verifier_dal::{via_store_mode_dal::StoreMode, via_transactions_dal::L1ToL2Transaction};
use zksync_object_store::{Bucket, MockObjectStore, ObjectStore, ObjectStoreError, StoredObject};
use zksync_types::{
    protocol_version::{ProtocolSemanticVersion, VersionPatch},
    ProtocolVersionId, H256,
};

use super::*;

fn fixture(name: &str) -> Vec<u8> {
    let crate_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lib/via_verification");
    std::env::set_var("VIA_VK_KEY_PATH", format!("{crate_dir}/keys"));
    std::fs::read(format!("{crate_dir}/examples/data/{name}")).unwrap()
}

fn v28() -> ProveBatchesV28 {
    bincode::deserialize(&fixture("data_batch_9_0_28_0.bin")).unwrap()
}

fn inscribed(data: &ProveBatchesV28) -> L1BatchDAReferenceInput {
    L1BatchDAReferenceInput {
        l1_batch_hash: data.l1_batches[0].metadata.root_hash,
        l1_batch_index: data.l1_batches[0].header.number,
        da_identifier: String::new(),
        blob_id: String::new(),
        prev_l1_batch_hash: data.prev_l1_batch.metadata.root_hash,
    }
}

fn v28_patches(patches: &[u32]) -> Vec<ProtocolSemanticVersion> {
    patches
        .iter()
        .map(|&patch| {
            ProtocolSemanticVersion::new(ProtocolVersionId::Version28, VersionPatch(patch))
        })
        .collect()
}

/// Checks a v28 package against its own inscription with patches 28.1 and 28.0 registered.
async fn check(
    store: &dyn ObjectStore,
    dev_mode: bool,
    data: &ProveBatchesV28,
    input: &L1BatchDAReferenceInput,
) -> anyhow::Result<Approval> {
    let bytes = bincode::serialize(data).unwrap();
    let allowed = v28_patches(&[1, 0]);
    verify_batch_proof(
        store,
        dev_mode,
        input,
        &allowed,
        ProtocolVersionId::Version28,
        &bytes,
    )
    .await
}

fn reason(outcome: anyhow::Result<Approval>) -> NoVerdictReason {
    outcome
        .unwrap_err()
        .downcast_ref::<NoVerdict>()
        .expect("a no-verdict error")
        .reason
}

/// Moves the embedded proof into the store under `version` and returns the package without it.
async fn omit_proof(store: &dyn ObjectStore, version: ProtocolSemanticVersion) -> ProveBatchesV28 {
    let mut data = v28();
    let proof = data.proofs.pop().unwrap();
    store
        .put((data.l1_batches[0].header.number, version), &proof)
        .await
        .unwrap();
    data
}

#[tokio::test]
async fn embedded_genuine_proofs_are_verified() {
    let store = MockObjectStore::arc();
    let data = v28();
    let approval = check(&*store, false, &data, &inscribed(&data))
        .await
        .unwrap();
    assert_eq!(approval, Approval::Verified);

    let v27: ProveBatchesV27 = bincode::deserialize(&fixture("data_batch_18_0_27_0.bin")).unwrap();
    let input = L1BatchDAReferenceInput {
        l1_batch_hash: v27.l1_batches[0].metadata.root_hash,
        l1_batch_index: v27.l1_batches[0].header.number,
        da_identifier: String::new(),
        blob_id: String::new(),
        prev_l1_batch_hash: v27.prev_l1_batch.metadata.root_hash,
    };
    let bytes = bincode::serialize(&v27).unwrap();
    let approval = verify_batch_proof(
        &*store,
        false,
        &input,
        &[],
        ProtocolVersionId::Version27,
        &bytes,
    )
    .await
    .unwrap();
    assert_eq!(approval, Approval::Verified);
}

#[tokio::test]
async fn a_package_for_another_batch_or_version_has_no_verdict() {
    let store = MockObjectStore::arc();
    let data = v28();
    let mut other_root = inscribed(&data);
    other_root.l1_batch_hash = H256::repeat_byte(1);
    let outcome = check(&*store, true, &data, &other_root).await;
    assert_eq!(reason(outcome), NoVerdictReason::MalformedPackage);

    // A declared version would otherwise choose which stored proofs are looked up.
    let mut other_version = omit_proof(&*store, v28_patches(&[0])[0]).await;
    other_version.l1_batches[0].header.protocol_version = Some(ProtocolVersionId::Version27);
    let outcome = check(&*store, true, &other_version, &inscribed(&other_version)).await;
    assert_eq!(reason(outcome), NoVerdictReason::MalformedPackage);
}

#[tokio::test]
async fn an_omitted_proof_is_found_under_an_older_patch() {
    let store = MockObjectStore::arc();
    let data = omit_proof(&*store, v28_patches(&[0])[0]).await;
    let approval = check(&*store, false, &data, &inscribed(&data))
        .await
        .unwrap();
    assert_eq!(approval, Approval::Verified);
}

#[tokio::test]
async fn an_absent_proof_waits_unless_development_mode_accepts_it() {
    let store = MockObjectStore::arc();
    let mut data = v28();
    data.proofs.clear();
    let input = inscribed(&data);

    let outcome = check(&*store, false, &data, &input).await;
    assert_eq!(reason(outcome), NoVerdictReason::ProofUnavailable);
    let approval = check(&*store, true, &data, &input).await.unwrap();
    assert_eq!(approval, Approval::DevAccepted);

    // Without a registered patch the lookup never ran, so absence is not established.
    let bytes = bincode::serialize(&data).unwrap();
    let outcome = verify_batch_proof(
        &*store,
        true,
        &input,
        &[],
        ProtocolVersionId::Version28,
        &bytes,
    )
    .await;
    assert_eq!(reason(outcome), NoVerdictReason::ProofUnavailable);
}

#[tokio::test]
async fn an_undecodable_stored_proof_has_no_verdict() {
    let store = MockObjectStore::arc();
    let mut data = v28();
    data.proofs.clear();
    let batch = data.l1_batches[0].header.number;
    let key = via_verification::version_28::types::L1BatchProofForL1::encode_key((
        batch,
        v28_patches(&[1])[0],
    ));
    store
        .put_raw(Bucket::ProofsFri, &key, vec![1, 2, 3])
        .await
        .unwrap();

    let outcome = check(&*store, true, &data, &inscribed(&data)).await;
    assert_eq!(reason(outcome), NoVerdictReason::MalformedPackage);
}

#[tokio::test]
async fn a_failing_proof_has_no_verdict_even_in_development_mode() {
    let store = MockObjectStore::arc();
    let mut data = v28();
    // The roots still match the inscription, and only the unbound commitment differs.
    data.l1_batches[0].metadata.commitment = H256::repeat_byte(7);
    let outcome = check(&*store, true, &data, &inscribed(&data)).await;
    assert_eq!(reason(outcome), NoVerdictReason::ProofFailed);
}

/// A store whose every read fails, as a broken credential or backend does.
#[derive(Debug)]
struct FailingStore;

#[async_trait::async_trait]
impl ObjectStore for FailingStore {
    async fn get_raw(&self, _: Bucket, _: &str) -> Result<Vec<u8>, ObjectStoreError> {
        Err(ObjectStoreError::Other {
            source: "permission denied".into(),
            is_retriable: false,
        })
    }

    async fn put_raw(&self, _: Bucket, _: &str, _: Vec<u8>) -> Result<(), ObjectStoreError> {
        unreachable!()
    }

    async fn remove_raw(&self, _: Bucket, _: &str) -> Result<(), ObjectStoreError> {
        unreachable!()
    }

    fn storage_prefix_raw(&self, _: Bucket) -> String {
        String::new()
    }
}

#[tokio::test]
async fn a_failing_store_is_not_absence_even_in_development_mode() {
    let mut data = v28();
    data.proofs.clear();
    let outcome = check(&FailingStore, true, &data, &inscribed(&data)).await;
    assert_eq!(reason(outcome), NoVerdictReason::ProofStoreError);
}

const BATCH: i64 = 1;

/// A store with one unverified batch and one indexed deposit, and pubdata settling one deposit successfully.
/// That deposit is the indexed one unless `mismatch` is set.
async fn approval_fixture(
    dev_mode: bool,
    mismatch: bool,
) -> (Connection<'static, Verifier>, H256, H256, Pubdata) {
    approval_fixture_in(
        ConnectionPool::<Verifier>::test_pool().await,
        dev_mode,
        mismatch,
    )
    .await
}

async fn approval_fixture_in(
    pool: ConnectionPool<Verifier>,
    dev_mode: bool,
    mismatch: bool,
) -> (Connection<'static, Verifier>, H256, H256, Pubdata) {
    let mut storage = pool.connection().await.unwrap();
    storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&StoreMode {
            proof_verification_dev_mode: dev_mode,
            bitcoin_network: "regtest".into(),
            bitcoin_genesis_hash: bitcoin::blockdata::constants::genesis_block(
                bitcoin::Network::Regtest,
            )
            .block_hash()
            .to_string(),
        })
        .await
        .unwrap();
    let proof_reveal_tx_id = H256::random();
    storage
        .via_votes_dal()
        .insert_votable_transaction(
            BATCH as u32,
            H256::random(),
            H256::random(),
            "da".into(),
            proof_reveal_tx_id,
            "proof_blob".into(),
            "pubdata_tx".into(),
            "pubdata_blob".into(),
            None,
        )
        .await
        .unwrap();
    let deposit = H256::random();
    storage
        .via_transactions_dal()
        .insert_transaction(L1ToL2Transaction {
            priority_id: 0,
            tx_id: H256::random(),
            receiver: "receiver".into(),
            value: 1,
            calldata: vec![],
            canonical_tx_hash: deposit,
            l1_block_number: 1,
        })
        .await
        .unwrap();
    let pubdata = Pubdata {
        user_logs: vec![L1MessengerL2ToL1Log {
            sender: H160::from_str(L2_BOOTLOADER_CONTRACT_ADDR).unwrap(),
            key: if mismatch { H256::random() } else { deposit },
            value: H256::from_low_u64_be(1),
            ..Default::default()
        }],
        l2_to_l1_messages: vec![],
    };
    (storage, proof_reveal_tx_id, deposit, pubdata)
}

/// The batch status, its development marker and the deposit status, as persisted.
async fn persisted(
    storage: &mut Connection<'_, Verifier>,
    deposit: H256,
) -> (Option<bool>, bool, Option<bool>) {
    let (status, dev): (Option<bool>, bool) = sqlx::query_as(
        "SELECT l1_batch_status, unverified_dev FROM via_votable_transactions WHERE l1_batch_number = $1",
    )
    .bind(BATCH)
    .fetch_one(storage.conn())
    .await
    .unwrap();
    let deposit_status: Option<bool> =
        sqlx::query_scalar("SELECT status FROM via_transactions WHERE canonical_tx_hash = $1")
            .bind(deposit.as_bytes())
            .fetch_one(storage.conn())
            .await
            .unwrap();
    (status, dev, deposit_status)
}

#[tokio::test]
async fn an_approval_records_the_batch_and_its_deposits() {
    for (dev_mode, approval) in [(false, Approval::Verified), (true, Approval::DevAccepted)] {
        let (mut storage, tx_id, deposit, pubdata) = approval_fixture(dev_mode, false).await;
        let committed = record_approval(&mut storage, BATCH, tx_id, approval, &pubdata, None, 1)
            .await
            .unwrap();
        assert!(committed);
        let marked = approval == Approval::DevAccepted;
        assert_eq!(
            persisted(&mut storage, deposit).await,
            (Some(true), marked, Some(true))
        );
    }
}

#[tokio::test]
async fn a_deposit_mismatch_writes_nothing() {
    let (mut storage, tx_id, deposit, pubdata) = approval_fixture(false, true).await;
    let outcome = record_approval(
        &mut storage,
        BATCH,
        tx_id,
        Approval::Verified,
        &pubdata,
        None,
        1,
    )
    .await;
    assert_eq!(
        reason(outcome.map(|_| Approval::Verified)),
        NoVerdictReason::DepositMismatch
    );
    assert_eq!(persisted(&mut storage, deposit).await, (None, false, None));
}

#[tokio::test]
async fn a_strict_store_rolls_back_a_development_approval() {
    let (mut storage, tx_id, deposit, pubdata) = approval_fixture(false, false).await;
    let outcome = record_approval(
        &mut storage,
        BATCH,
        tx_id,
        Approval::DevAccepted,
        &pubdata,
        None,
        1,
    )
    .await;
    assert!(outcome.is_err());
    assert_eq!(persisted(&mut storage, deposit).await, (None, false, None));
}

#[tokio::test]
async fn a_reorg_before_commit_writes_nothing() {
    let (mut storage, tx_id, deposit, pubdata) = approval_fixture(false, false).await;
    storage
        .via_l1_block_dal()
        .insert_reorg_metadata(1, BATCH)
        .await
        .unwrap();
    let committed = record_approval(
        &mut storage,
        BATCH,
        tx_id,
        Approval::Verified,
        &pubdata,
        None,
        1,
    )
    .await
    .unwrap();
    assert!(!committed);
    assert_eq!(persisted(&mut storage, deposit).await, (None, false, None));
}

#[tokio::test]
async fn watermarks_come_from_the_database_and_follow_a_reorg() {
    let (mut storage, tx_id, _, pubdata) = approval_fixture(false, false).await;
    // A fresh process sees the pending batch without having processed anything.
    assert_eq!(report_watermarks(&mut storage).await.unwrap(), (1, 0));

    record_approval(
        &mut storage,
        BATCH,
        tx_id,
        Approval::Verified,
        &pubdata,
        None,
        1,
    )
    .await
    .unwrap();
    assert_eq!(report_watermarks(&mut storage).await.unwrap(), (1, 1));

    // A reorg removes the batch, and both watermarks fall back with it.
    sqlx::query("DELETE FROM via_votable_transactions WHERE l1_batch_number = $1")
        .bind(BATCH)
        .execute(storage.conn())
        .await
        .unwrap();
    assert_eq!(report_watermarks(&mut storage).await.unwrap(), (0, 0));
}

/// Only the RPC transport and DA/object storage are doubled; the inscription parser,
/// proof verifier, selection, deposit checks and transaction writes remain production code.
#[derive(Debug, Clone)]
struct FixtureDa {
    proof: Vec<u8>,
    pubdata: Vec<u8>,
}

#[async_trait::async_trait]
impl DataAvailabilityClient for FixtureDa {
    async fn dispatch_blob(
        &self,
        _: u32,
        _: Vec<u8>,
    ) -> Result<zksync_da_client::types::DispatchResponse, zksync_da_client::types::DAError> {
        unreachable!("verifier must not publish DA")
    }

    async fn get_inclusion_data(
        &self,
        id: &str,
    ) -> Result<Option<InclusionData>, zksync_da_client::types::DAError> {
        let data = match id {
            "proof" => &self.proof,
            "pubdata" => &self.pubdata,
            other => panic!("unexpected DA blob {other}"),
        };
        Ok(Some(InclusionData { data: data.clone() }))
    }

    fn clone_boxed(&self) -> Box<dyn DataAvailabilityClient> {
        Box::new(self.clone())
    }

    fn blob_size_limit(&self) -> Option<usize> {
        None
    }
}

struct LifecycleFixture {
    verifier: ViaVerifier,
    store: Arc<dyn ObjectStore>,
    deposit: H256,
    rpc: RpcServer,
    client: Arc<BitcoinClient>,
    endpoint: String,
}

struct RpcServer(tokio::task::JoinHandle<()>);

impl Drop for RpcServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn lifecycle_fixture(dev_mode: bool, mismatch: bool) -> LifecycleFixture {
    lifecycle_fixture_with_genesis(dev_mode, mismatch, bitcoin::Network::Regtest).await
}

async fn lifecycle_fixture_with_genesis(
    dev_mode: bool,
    mismatch: bool,
    genesis_network: bitcoin::Network,
) -> LifecycleFixture {
    use bitcoin::consensus::encode::serialize_hex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use via_btc_client::{
        inscriber::test_utils::{get_mock_inscriber_and_conditions, MockBitcoinOpsConfig},
        types::{InscriptionMessage, NodeAuth, ProofDAReferenceInput},
    };
    use zksync_types::via_wallet::SystemWalletsDetails;

    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let (mut storage, _, deposit, pubdata) =
        approval_fixture_in(pool.clone(), dev_mode, mismatch).await;
    let mut package = v28();
    package.proofs.clear();
    let batch = package.l1_batches[0].header.number.0;
    let mut inscriber = get_mock_inscriber_and_conditions(MockBitcoinOpsConfig::default());
    let mut input = inscribed(&package);
    input.blob_id = "pubdata".into();
    input.da_identifier = "fixture".into();
    let batch_tx = inscriber
        .prepare_inscribe(&InscriptionMessage::L1BatchDAReference(input.clone()), None)
        .await
        .unwrap()
        .final_reveal_tx
        .tx;
    let proof_tx = inscriber
        .prepare_inscribe(
            &InscriptionMessage::ProofDAReference(ProofDAReferenceInput {
                l1_batch_reveal_txid: batch_tx.compute_txid(),
                da_identifier: "fixture".into(),
                blob_id: "proof".into(),
            }),
            None,
        )
        .await
        .unwrap()
        .final_reveal_tx
        .tx;
    let address =
        bitcoin::Address::from_script(&batch_tx.output[0].script_pubkey, bitcoin::Network::Regtest)
            .unwrap();
    let wallets = SystemWallets {
        bridge: address.clone(),
        sequencer: address.clone(),
        governance: address.clone(),
        verifiers: vec![address.clone()],
    };
    storage
        .via_wallet_dal()
        .insert_wallets(&SystemWalletsDetails::try_from(wallets.clone()).unwrap(), 1)
        .await
        .unwrap();
    storage
        .via_indexer_dal()
        .init_indexer_metadata("via_btc_watch", 1)
        .await
        .unwrap();
    let version = v28_patches(&[0])[0];
    storage
        .via_protocol_versions_dal()
        .save_protocol_version(version, &[0; 32], &[0; 32], &[0; 32], &[0; 32])
        .await
        .unwrap();
    sqlx::query("UPDATE protocol_versions SET executed = TRUE")
        .execute(storage.conn())
        .await
        .unwrap();
    let proof_hash = H256::from_str(&proof_tx.compute_txid().to_string()).unwrap();
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_number = $1, l1_batch_hash = $2, prev_l1_batch_hash = $3, proof_reveal_tx_id = $4")
        .bind(i64::from(batch)).bind(input.l1_batch_hash.as_bytes())
        .bind(input.prev_l1_batch_hash.as_bytes()).bind(proof_hash.as_bytes())
        .execute(storage.conn()).await.unwrap();
    storage
        .via_votes_dal()
        .insert_votable_transaction(
            batch - 1,
            input.prev_l1_batch_hash,
            H256::zero(),
            "fixture".into(),
            H256::repeat_byte(8),
            "previous".into(),
            "previous".into(),
            "previous".into(),
            None,
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE via_votable_transactions SET l1_batch_status = TRUE WHERE l1_batch_number = $1",
    )
    .bind(i64::from(batch - 1))
    .execute(storage.conn())
    .await
    .unwrap();
    sqlx::query("INSERT INTO via_withdrawal_holds(wallet, reference, reason, source) VALUES ($1, 'keep', 'unresolved', $2)")
        .bind(address.script_pubkey().as_bytes()).bind(&[7_u8; 32][..])
        .execute(storage.conn()).await.unwrap();
    storage
        .via_votes_dal()
        .insert_votable_transaction(
            batch + 1,
            H256::repeat_byte(10),
            input.l1_batch_hash,
            "fixture".into(),
            H256::repeat_byte(10),
            "next_proof".into(),
            "next_tx".into(),
            "next_blob".into(),
            None,
        )
        .await
        .unwrap();
    let id = storage
        .via_votes_dal()
        .get_votable_transaction_id(proof_hash.as_bytes())
        .await
        .unwrap()
        .unwrap();
    storage
        .via_votes_dal()
        .insert_vote(id, &address.to_string(), true, None)
        .await
        .unwrap();
    storage
        .via_protocol_versions_dal()
        .save_protocol_version(
            ProtocolSemanticVersion::new(ProtocolVersionId::Version27, VersionPatch(0)),
            &[0; 32],
            &[0; 32],
            &[6; 32],
            &[0; 32],
        )
        .await
        .unwrap();
    drop(storage);

    let transactions = std::collections::HashMap::from([
        (
            batch_tx.compute_txid().to_string(),
            serialize_hex(&batch_tx),
        ),
        (
            proof_tx.compute_txid().to_string(),
            serialize_hex(&proof_tx),
        ),
    ]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let genesis = bitcoin::blockdata::constants::genesis_block(genesis_network);
    let rpc = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let (header_end, length) = loop {
                let mut buffer = [0; 4096];
                let read = stream.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0);
                bytes.extend_from_slice(&buffer[..read]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while bytes.len() < header_end + length {
                let mut buffer = [0; 4096];
                let read = stream.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0);
                bytes.extend_from_slice(&buffer[..read]);
            }
            let request: serde_json::Value =
                serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "getblockcount" => serde_json::json!(1),
                "getblockhash" => serde_json::json!(genesis.block_hash().to_string()),
                "getblock" => serde_json::json!(serialize_hex(&genesis)),
                "getrawtransaction" => {
                    serde_json::json!(transactions[request["params"][0].as_str().unwrap()])
                }
                method => panic!("unexpected Bitcoin RPC {method}"),
            };
            let body = serde_json::json!({"result": result, "error": null, "id": request["id"]})
                .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let client = Arc::new(
        BitcoinClient::new(
            &endpoint,
            NodeAuth::None,
            zksync_config::configs::via_btc_client::ViaBtcClientConfig {
                network: "regtest".into(),
                external_apis: vec![],
                fee_strategies: vec![],
                use_rpc_for_fee_rate: Some(true),
            },
        )
        .unwrap(),
    );
    let indexer = BitcoinInscriptionIndexer::new(client.clone(), Arc::new(wallets));
    let mut config = ViaVerifierConfig::for_tests();
    config.wallet_address = address.to_string();
    config.proof_verification_dev_mode = dev_mode;
    let store = MockObjectStore::arc();
    let verifier = ViaVerifier::new(
        config,
        indexer,
        pool,
        Box::new(FixtureDa {
            proof: bincode::serialize(&package).unwrap(),
            pubdata: pubdata.encode_pubdata(),
        }),
        client.clone(),
        ViaBtcWatchConfig::for_tests(),
        store.clone(),
    )
    .await
    .unwrap();
    LifecycleFixture {
        verifier,
        store,
        deposit,
        rpc: RpcServer(rpc),
        client,
        endpoint,
    }
}

async fn lifecycle_state(fixture: &LifecycleFixture) -> (Option<bool>, bool, Option<bool>, i64) {
    let mut storage = fixture.verifier.pool.connection().await.unwrap();
    let (status, dev) = sqlx::query_as::<_, (Option<bool>, bool)>(
        "SELECT l1_batch_status, unverified_dev FROM via_votable_transactions WHERE l1_batch_number = 9",
    ).fetch_one(storage.conn()).await.unwrap();
    let deposit = sqlx::query_scalar::<_, Option<bool>>(
        "SELECT status FROM via_transactions WHERE canonical_tx_hash = $1",
    )
    .bind(fixture.deposit.as_bytes())
    .fetch_one(storage.conn())
    .await
    .unwrap();
    let holds = sqlx::query_scalar("SELECT COUNT(*) FROM via_withdrawal_holds WHERE reference = 'keep' AND reason = 'unresolved'")
        .fetch_one(storage.conn()).await.unwrap();
    (status, dev, deposit, holds)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_proof_retries_the_same_persisted_batch_then_approves() {
    let mut fixture = lifecycle_fixture(false, false).await;
    let mut storage = fixture.verifier.pool.connection().await.unwrap();
    let selected = storage
        .via_votes_dal()
        .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
        .await
        .unwrap();
    assert_eq!(selected.as_ref().map(|batch| batch.0), Some(9));
    let before = durable_effects(&mut storage).await;
    let result = fixture.verifier.loop_iteration(&mut storage).await;
    assert_eq!(
        reason(result.map(|_| Approval::Verified)),
        NoVerdictReason::ProofUnavailable
    );
    assert_eq!(lifecycle_state(&fixture).await, (None, false, None, 1));
    assert_eq!(durable_effects(&mut storage).await, before);
    assert_eq!(
        storage
            .via_votes_dal()
            .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
            .await
            .unwrap(),
        selected
    );
    assert_eq!(report_watermarks(&mut storage).await.unwrap(), (10, 8));
    // Reconnect as on restart: the pending work comes from the database, not memory.
    drop(storage);
    let mut storage = fixture.verifier.pool.connection().await.unwrap();
    let mut package = v28();
    fixture
        .store
        .put(
            (package.l1_batches[0].header.number, v28_patches(&[0])[0]),
            &package.proofs.pop().unwrap(),
        )
        .await
        .unwrap();
    fixture.verifier.loop_iteration(&mut storage).await.unwrap();
    assert_eq!(
        lifecycle_state(&fixture).await,
        (Some(true), false, Some(true), 1)
    );
    assert_eq!(report_watermarks(&mut storage).await.unwrap(), (10, 9));
    assert_eq!(
        storage
            .via_votes_dal()
            .get_first_not_verified_l1_batch_in_canonical_inscription_chain()
            .await
            .unwrap()
            .map(|row| row.0),
        Some(10)
    );
    assert_eq!(
        storage
            .via_votes_dal()
            .get_verifier_vote_status(1)
            .await
            .unwrap()
            .map(|row| row.0),
        Some(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn development_absence_does_not_bypass_deposit_mismatch() {
    let mut fixture = lifecycle_fixture(true, true).await;
    let mut storage = fixture.verifier.pool.connection().await.unwrap();
    let before = durable_effects(&mut storage).await;
    let result = fixture.verifier.loop_iteration(&mut storage).await;
    assert_eq!(
        reason(result.map(|_| Approval::Verified)),
        NoVerdictReason::DepositMismatch
    );
    assert_eq!(lifecycle_state(&fixture).await, (None, false, None, 1));
    assert_eq!(durable_effects(&mut storage).await, before);
    assert_eq!(report_watermarks(&mut storage).await.unwrap(), (10, 8));
}

async fn durable_effects(storage: &mut Connection<'_, Verifier>) -> Vec<serde_json::Value> {
    let mut state = Vec::new();
    for table in [
        "via_votable_transactions",
        "via_votes",
        "via_transactions",
        "via_withdrawal_holds",
        "protocol_versions",
        "via_l1_batch_vote_inscription_request",
    ] {
        let query = format!(
            "SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text), '[]'::jsonb) FROM {table} t"
        );
        state.push(
            sqlx::query_scalar(&query)
                .fetch_one(storage.conn())
                .await
                .unwrap(),
        );
    }
    state
}

#[tokio::test]
async fn a_later_database_error_rolls_back_the_development_marker_and_effects() {
    let (mut storage, tx_id, deposit, pubdata) = approval_fixture(true, false).await;
    let upgrade = H256::repeat_byte(6);
    storage
        .via_protocol_versions_dal()
        .save_protocol_version(
            v28_patches(&[0])[0],
            &[0; 32],
            &[0; 32],
            upgrade.as_bytes(),
            &[0; 32],
        )
        .await
        .unwrap();
    storage
        .via_votes_dal()
        .insert_vote(1, "verifier", true, None)
        .await
        .unwrap();
    sqlx::query(
        "ALTER TABLE protocol_versions ADD CONSTRAINT fixture_upgrade_failure CHECK (NOT executed)",
    )
    .execute(storage.conn())
    .await
    .unwrap();
    let before = durable_effects(&mut storage).await;
    record_approval(
        &mut storage,
        BATCH,
        tx_id,
        Approval::DevAccepted,
        &pubdata,
        Some(upgrade),
        1,
    )
    .await
    .unwrap_err();
    assert_eq!(persisted(&mut storage, deposit).await, (None, false, None));
    assert_eq!(durable_effects(&mut storage).await, before);

    sqlx::query("ALTER TABLE protocol_versions DROP CONSTRAINT fixture_upgrade_failure")
        .execute(storage.conn())
        .await
        .unwrap();
    assert!(record_approval(
        &mut storage,
        BATCH,
        tx_id,
        Approval::DevAccepted,
        &pubdata,
        Some(upgrade),
        1,
    )
    .await
    .unwrap());
    assert_eq!(
        persisted(&mut storage, deposit).await,
        (Some(true), true, Some(true))
    );
    let finalized: bool =
        sqlx::query_scalar("SELECT is_finalized FROM via_votable_transactions WHERE id = 1")
            .fetch_one(storage.conn())
            .await
            .unwrap();
    assert!(finalized);
    assert_eq!(
        storage
            .via_protocol_versions_dal()
            .get_in_progress_upgrade_tx_hash()
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_development_reorg_rolls_back_the_marker_and_deposit_effects() {
    let (mut storage, tx_id, deposit, pubdata) = approval_fixture(true, false).await;
    storage
        .via_l1_block_dal()
        .insert_reorg_metadata(1, BATCH)
        .await
        .unwrap();
    let before = durable_effects(&mut storage).await;
    assert!(!record_approval(
        &mut storage,
        BATCH,
        tx_id,
        Approval::DevAccepted,
        &pubdata,
        None,
        1,
    )
    .await
    .unwrap());
    assert_eq!(persisted(&mut storage, deposit).await, (None, false, None));
    assert_eq!(durable_effects(&mut storage).await, before);
}

#[derive(Debug)]
struct WaitingStore(Arc<tokio::sync::Notify>);

#[async_trait::async_trait]
impl ObjectStore for WaitingStore {
    async fn get_raw(&self, _: Bucket, _: &str) -> Result<Vec<u8>, ObjectStoreError> {
        self.0.notify_one();
        std::future::pending().await
    }

    async fn put_raw(&self, _: Bucket, _: &str, _: Vec<u8>) -> Result<(), ObjectStoreError> {
        unreachable!()
    }

    async fn remove_raw(&self, _: Bucket, _: &str) -> Result<(), ObjectStoreError> {
        unreachable!()
    }

    fn storage_prefix_raw(&self, _: Bucket) -> String {
        String::new()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_interrupts_a_yielding_proof_fetch_without_a_verdict() {
    let mut fixture = lifecycle_fixture(false, false).await;
    let mut observer = fixture.verifier.pool.connection().await.unwrap();
    let before = durable_effects(&mut observer).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    fixture.verifier.blob_store = Arc::new(WaitingStore(entered.clone()));
    let (stop, receiver) = watch::channel(false);
    let run = fixture.verifier.run(receiver);
    tokio::pin!(run);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            result = &mut run => panic!("verifier exited before proof fetch: {result:?}"),
            _ = entered.notified() => {}
        }
    })
    .await
    .unwrap();
    assert_eq!(report_watermarks(&mut observer).await.unwrap(), (10, 8));
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(durable_effects(&mut observer).await, before);
    drop(fixture.rpc);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_interrupts_connection_acquisition() {
    let mut fixture = lifecycle_fixture(false, false).await;
    let pool = ConnectionPool::<Verifier>::singleton(fixture.verifier.pool.database_url().clone())
        .build()
        .await
        .unwrap();
    let mut held = pool.connection().await.unwrap();
    let before = durable_effects(&mut held).await;
    fixture.verifier.pool = pool;
    let (stop, receiver) = watch::channel(false);
    let run = fixture.verifier.run(receiver);
    tokio::pin!(run);
    std::future::poll_fn(|cx| {
        assert!(run.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    // Register this later timer only after run has registered its first tick.
    // Poll run first when it expires, so the earlier tick reaches the exhausted pool before stop.
    tokio::select! {
        biased;
        result = &mut run => panic!("verifier exited while the pool was exhausted: {result:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
    }
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(durable_effects(&mut held).await, before);
    drop(fixture.rpc);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_during_a_post_marker_database_wait_rolls_back_all_effects() {
    let fixture = lifecycle_fixture(true, false).await;
    let pool = fixture.verifier.pool.clone();
    let mut observer = pool.connection().await.unwrap();
    let mut blocker = pool.connection().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.conn())
        .await
        .unwrap();
    let before = durable_effects(&mut observer).await;
    let mut lock = blocker.start_transaction().await.unwrap();
    sqlx::query("SELECT canonical_tx_hash FROM via_transactions FOR UPDATE")
        .fetch_all(lock.conn())
        .await
        .unwrap();
    let (stop, receiver) = watch::channel(false);
    let run = fixture.verifier.run(receiver);
    tokio::pin!(run);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            tokio::select! {
                result = &mut run => panic!("verifier exited before its deposit write: {result:?}"),
                waiting = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() \
                     AND $1 = ANY(pg_blocking_pids(pid)) AND query ILIKE '%UPDATE%via_transactions%')"
                ).bind(blocker_pid).fetch_one(observer.conn()) => {
                    if waiting.unwrap() { break; }
                }
            }
        }
    }).await.unwrap();
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut run)
        .await
        .unwrap()
        .unwrap();
    lock.commit().await.unwrap();
    // Wait for the cancelled write and queued rollback to release their row lock.
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        sqlx::query("SELECT canonical_tx_hash FROM via_transactions FOR UPDATE")
            .fetch_all(observer.conn()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(durable_effects(&mut observer).await, before);
    drop(fixture.rpc);
}

async fn initialize(
    pool: ConnectionPool<Verifier>,
    client: Arc<BitcoinClient>,
    dev_mode: bool,
) -> anyhow::Result<via_verifier_storage_init::ViaVerifierStorageInitializer> {
    via_verifier_storage_init::ViaVerifierStorageInitializer::new(
        pool,
        client,
        zksync_config::configs::via_consensus::ViaGenesisConfig::default(),
        ViaBtcWatchConfig::for_tests(),
        dev_mode,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialized_development_store_reopens_but_refuses_a_strict_process() {
    let mut fixture = lifecycle_fixture(true, false).await;
    let database_url = fixture.verifier.pool.database_url().clone();
    let mut storage = fixture.verifier.pool.connection().await.unwrap();
    fixture.verifier.loop_iteration(&mut storage).await.unwrap();
    assert_eq!(
        lifecycle_state(&fixture).await,
        (Some(true), true, Some(true), 1)
    );
    let before = durable_effects(&mut storage).await;
    drop(storage);
    drop(fixture.verifier);
    let restarted = ConnectionPool::<Verifier>::singleton(database_url)
        .build()
        .await
        .unwrap();
    initialize(restarted.clone(), fixture.client.clone(), true)
        .await
        .unwrap();
    assert!(initialize(restarted.clone(), fixture.client.clone(), false)
        .await
        .is_err());
    let mut storage = restarted.connection().await.unwrap();
    assert_eq!(durable_effects(&mut storage).await, before);
    assert_eq!(
        storage
            .via_votes_dal()
            .get_verifier_vote_status(1)
            .await
            .unwrap()
            .map(|row| row.0),
        Some(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initializer_checks_actual_genesis_and_network_before_designating_the_store() {
    let fixture = lifecycle_fixture_with_genesis(false, false, bitcoin::Network::Bitcoin).await;
    for network in ["regtest", "malformed-network", "bitcoin"] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut config = fixture.client.config.clone();
        config.network = network.into();
        let client = Arc::new(
            BitcoinClient::new(
                &fixture.endpoint,
                via_btc_client::types::NodeAuth::None,
                config,
            )
            .unwrap(),
        );
        assert!(initialize(pool.clone(), client, true).await.is_err());
        let mut storage = pool.connection().await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM via_verifier_store_mode")
            .fetch_one(storage.conn())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
    // The configured regtest endpoint now serves Bitcoin genesis, even for an initialized store.
    assert!(
        initialize(fixture.verifier.pool.clone(), fixture.client.clone(), false)
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_bootstrap_keeps_the_committed_strict_designation() {
    let fixture = lifecycle_fixture(false, false).await;
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    initialize(pool.clone(), fixture.client.clone(), false)
        .await
        .unwrap_err();
    let mut storage = pool.connection().await.unwrap();
    let mode: (bool, String, String) = sqlx::query_as(
        "SELECT proof_verification_dev_mode, bitcoin_network, bitcoin_genesis_hash FROM via_verifier_store_mode",
    ).fetch_one(storage.conn()).await.unwrap();
    assert_eq!(
        mode,
        (
            false,
            "regtest".into(),
            bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest)
                .block_hash()
                .to_string()
        )
    );
    assert!(initialize(pool, fixture.client.clone(), true)
        .await
        .is_err());
}
