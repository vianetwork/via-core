//! Outcomes of `verify_batch_proof` for genuine v27 and v28 proof packages, and of `record_approval` in a database.

use std::future::Future;

use via_btc_client::{
    types::{L1BatchDAReferenceInput, ProofDAReferenceInput},
    indexer::replay::{Included, MessageOrigin, Payload, ProofInput},
};
use via_test_utils::{bitcoin_rpc::BitcoinRpcServer, chain::Chain};
use zksync_da_client::types::InclusionData;
use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};
use via_da_client::types::L1MessengerL2ToL1Log;
use via_verification::{
    version_27::types::ProveBatches as ProveBatchesV27,
    version_28::types::ProveBatches as ProveBatchesV28,
};
use via_verifier_dal::via_store_mode_dal::StoreMode;
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
    verify_batch_proof(store, dev_mode, input, &allowed, ProtocolVersionId::Version28, &bytes).await
}

fn reason(outcome: anyhow::Result<Approval>) -> NoVerdictReason {
    outcome.unwrap_err().downcast_ref::<NoVerdict>().expect("a no-verdict error").reason
}

/// Moves the embedded proof into the store under `version` and returns the package without it.
async fn omit_proof(store: &dyn ObjectStore, version: ProtocolSemanticVersion) -> ProveBatchesV28 {
    let mut data = v28();
    let proof = data.proofs.pop().unwrap();
    store.put((data.l1_batches[0].header.number, version), &proof).await.unwrap();
    data
}

#[tokio::test]
async fn embedded_genuine_proofs_are_verified() {
    let store = MockObjectStore::arc();
    let data = v28();
    let approval = check(&*store, false, &data, &inscribed(&data)).await.unwrap();
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
    let approval =
        verify_batch_proof(&*store, false, &input, &[], ProtocolVersionId::Version27, &bytes)
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

    // Declared version would otherwise choose which stored proofs are looked up.
    let mut other_version = omit_proof(&*store, v28_patches(&[0])[0]).await;
    other_version.l1_batches[0].header.protocol_version = Some(ProtocolVersionId::Version27);
    let outcome = check(&*store, true, &other_version, &inscribed(&other_version)).await;
    assert_eq!(reason(outcome), NoVerdictReason::MalformedPackage);
}

#[tokio::test]
async fn an_omitted_proof_is_found_under_an_older_patch() {
    let store = MockObjectStore::arc();
    let data = omit_proof(&*store, v28_patches(&[0])[0]).await;
    let approval = check(&*store, false, &data, &inscribed(&data)).await.unwrap();
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

    // Absence is not established, because lookup never ran without registered patch.
    let bytes = bincode::serialize(&data).unwrap();
    let outcome =
        verify_batch_proof(&*store, true, &input, &[], ProtocolVersionId::Version28, &bytes).await;
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
    store.put_raw(Bucket::ProofsFri, &key, vec![1, 2, 3]).await.unwrap();

    let outcome = check(&*store, true, &data, &inscribed(&data)).await;
    assert_eq!(reason(outcome), NoVerdictReason::MalformedPackage);
}

#[tokio::test]
async fn a_failing_proof_has_no_verdict_even_in_development_mode() {
    let store = MockObjectStore::arc();
    let mut data = v28();
    // Roots match inscription, and only unbound commitment differs.
    data.l1_batches[0].metadata.commitment = H256::repeat_byte(7);
    let outcome = check(&*store, true, &data, &inscribed(&data)).await;
    assert_eq!(reason(outcome), NoVerdictReason::ProofFailed);
}

/// A store whose every read fails, as a broken credential or backend does.
#[derive(Debug)]
enum TestProofStore {
    Failing,
    Waiting(Arc<tokio::sync::Notify>),
}

#[async_trait::async_trait]
impl ObjectStore for TestProofStore {
    async fn get_raw(&self, _: Bucket, _: &str) -> Result<Vec<u8>, ObjectStoreError> {
        match self {
            Self::Failing => Err(ObjectStoreError::Other {
                source: "permission denied".into(),
                is_retriable: false,
            }),
            Self::Waiting(entered) => {
                entered.notify_one();
                std::future::pending().await
            }
        }
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
    let outcome = check(&TestProofStore::Failing, true, &data, &inscribed(&data)).await;
    assert_eq!(reason(outcome), NoVerdictReason::ProofStoreError);
}

const BATCH: i64 = 1;

/// A store with one unverified batch and one indexed deposit, and pubdata settling one deposit successfully.
/// That deposit is the indexed one unless `mismatch` is set.
async fn approval_fixture(
    dev_mode: bool,
    mismatch: bool,
) -> (Connection<'static, Verifier>, ProofWork, H256, Pubdata) {
    let (mut storage, deposit, pubdata) =
        approval_store(ConnectionPool::<Verifier>::test_pool().await, dev_mode, mismatch).await;
    let mut batch = inscribed(&v28());
    batch.l1_batch_index = zksync_types::L1BatchNumber(BATCH as u32);
    let proof = ProofDAReferenceInput {
        l1_batch_reveal_txid: "01".repeat(32).parse().unwrap(),
        da_identifier: "da".into(),
        blob_id: "proof".into(),
    };
    let work = materialize_work(
        &mut storage,
        "02".repeat(32).parse().unwrap(),
        batch,
        proof,
        via_test_utils::utils::test_wallets(),
    )
    .await;
    (storage, work, deposit, pubdata)
}

async fn materialize_work(
    storage: &mut Connection<'_, Verifier>,
    txid: bitcoin::Txid,
    batch: L1BatchDAReferenceInput,
    proof: ProofDAReferenceInput,
    wallets: SystemWallets,
) -> ProofWork {
    let chain = Chain::with_height(1);
    let mut tx = storage.start_transaction().await.unwrap();
    via_test_utils::reader_at(&mut tx, &chain, 1, 1, bitcoin::Network::Regtest).await.unwrap();
    sqlx::query("DELETE FROM via_btc_consumers WHERE name='withdrawals'")
        .execute(tx.conn())
        .await
        .unwrap();
    sqlx::query("UPDATE via_btc_reader SET replay_ready=FALSE").execute(tx.conn()).await.unwrap();
    let mut reader = ReaderDal::new(&mut tx);
    reader
        .insert_wallets(
            &SystemWalletsDetails::try_from(wallets).unwrap(),
            via_btc_storage::WalletSource::Unbound(0),
        )
        .await
        .unwrap();
    let block = reader.header_at(1).await.unwrap().unwrap();
    let origin = MessageOrigin {
        height: 1,
        hash: block.header.block_hash,
        transaction_index: 0,
        message_index: 0,
        txid: proof.l1_batch_reveal_txid,
    };
    let input = Included {
        origin: MessageOrigin { transaction_index: 1, txid, ..origin.clone() },
        payload: source_payload(ProofInput {
            proof,
            pubdata: Included { origin, payload: source_payload(batch) },
        }),
    };
    tx.via_votes_dal().save_reader_proof(&input, block.id).await.unwrap();
    let raw = via_btc_client::indexer::replay::transaction_hash(txid);
    let id = tx.via_votes_dal().get_votable_transaction_id(raw.as_bytes()).await.unwrap().unwrap();
    tx.commit().await.unwrap();
    ProofWork { id, header_id: block.id, proof: input }
}

async fn approval_store(
    pool: ConnectionPool<Verifier>,
    dev_mode: bool,
    mismatch: bool,
) -> (Connection<'static, Verifier>, H256, Pubdata) {
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
    let input = via_btc_client::types::L1ToL2MessageInput {
        receiver_l2_address: H160::repeat_byte(7),
        l2_contract_address: H160::repeat_byte(7),
        call_data: vec![],
    };
    let operation = zksync_types::l1::via_l1::ViaL1Deposit {
        l2_receiver_address: input.receiver_l2_address,
        amount: 5_000,
        calldata: vec![],
        l1_block_number: 1,
        tx_index: 0,
        output_vout: 0,
    }
    .l1_tx()
    .unwrap();
    let deposit = operation.common_data.canonical_tx_hash;
    storage
        .via_transactions_dal()
        .insert_transaction(
            via_verifier_dal::via_transactions_dal::L1ToL2Transaction {
                priority_id: operation.common_data.serial_id.0 as i64,
                l1_block_number: 1,
                tx_id: H256::random(),
                receiver: format!("{:#x}", input.receiver_l2_address),
                value: 5_000,
                calldata: vec![],
                canonical_tx_hash: deposit,
            },
            &via_btc_client::indexer::replay::DepositOrigin {
                height: 1,
                hash: Chain::with_height(1).blocks[&1].block_hash(),
                vout: 0,
            },
            &source_payload((input, 5_000)),
        )
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
    (storage, deposit, pubdata)
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
        let committed =
            record_approval(&mut storage, &tx_id, approval, &pubdata, None).await.unwrap();
        assert!(committed);
        let marked = approval == Approval::DevAccepted;
        assert_eq!(persisted(&mut storage, deposit).await, (Some(true), marked, Some(true)));
    }
}

#[tokio::test]
async fn a_deposit_mismatch_writes_nothing() {
    let (mut storage, tx_id, deposit, pubdata) = approval_fixture(false, true).await;
    let outcome = record_approval(&mut storage, &tx_id, Approval::Verified, &pubdata, None).await;
    assert_eq!(reason(outcome.map(|_| Approval::Verified)), NoVerdictReason::DepositMismatch);
    assert_eq!(persisted(&mut storage, deposit).await, (None, false, None));
}

#[tokio::test]
async fn a_strict_store_rolls_back_a_development_approval() {
    let (mut storage, tx_id, deposit, pubdata) = approval_fixture(false, false).await;
    let outcome =
        record_approval(&mut storage, &tx_id, Approval::DevAccepted, &pubdata, None).await;
    assert!(outcome.is_err());
    assert_eq!(persisted(&mut storage, deposit).await, (None, false, None));
}

async fn assert_work_held(
    storage: &mut Connection<'_, Verifier>,
    work: &ProofWork,
    pubdata: &Pubdata,
    deposit: H256,
) {
    assert!(!record_approval(storage, work, Approval::Verified, pubdata, None).await.unwrap());
    assert_eq!(persisted(storage, deposit).await, (None, false, None));
}

#[tokio::test]
async fn incomplete_protocol_prefix_holds_approval() {
    let (mut storage, old, deposit, pubdata) = approval_fixture(false, false).await;
    sqlx::query(
        "UPDATE via_btc_consumers SET header_id=(SELECT id FROM via_btc_headers WHERE number=0)",
    )
    .execute(storage.conn())
    .await
    .unwrap();
    assert_work_held(&mut storage, &old, &pubdata, deposit).await;
}

async fn approve_and_assert_state(
    storage: &mut Connection<'_, Verifier>,
    work: &ProofWork,
    pubdata: &Pubdata,
    deposit: H256,
) {
    assert!(record_approval(storage, work, Approval::Verified, pubdata, None).await.unwrap());
    assert_eq!(persisted(storage, deposit).await, (Some(true), false, Some(true)));
}

#[tokio::test]
async fn stale_work_cannot_complete_and_fresh_work_completes_once() {
    let (mut storage, old, deposit, pubdata) = approval_fixture(false, false).await;
    // Rebuild exactly same source occurrence. BIGSERIAL identities are never reset.
    sqlx::query("DELETE FROM via_votable_transactions WHERE id=$1")
        .bind(old.id)
        .execute(storage.conn())
        .await
        .unwrap();
    let new = materialize_work(
        &mut storage,
        old.proof.origin.txid,
        old.proof.payload.input.pubdata.payload.input.clone(),
        old.proof.payload.input.proof.clone(),
        via_test_utils::utils::test_wallets(),
    )
    .await;
    assert_ne!(old.id, new.id);
    assert_work_held(&mut storage, &old, &pubdata, deposit).await;
    approve_and_assert_state(&mut storage, &new, &pubdata, deposit).await;
    assert!(!record_approval(&mut storage, &new, Approval::Verified, &pubdata, None)
        .await
        .unwrap());
}

#[tokio::test]
async fn distinct_same_batch_proof_can_record_approval() {
    let (mut storage, new, deposit, pubdata) = approval_fixture(false, false).await;
    assert!(record_approval(&mut storage, &new, Approval::Verified, &pubdata, None).await.unwrap());
    let mut repeated = new.proof.clone();
    repeated.origin.txid = "04".repeat(32).parse().unwrap();
    repeated.origin.transaction_index += 1;
    let mut tx = storage.start_transaction().await.unwrap();
    tx.via_votes_dal().save_reader_proof(&repeated, new.header_id).await.unwrap();
    tx.commit().await.unwrap();
    let second = storage.via_votes_dal().pending_reader_proof().await.unwrap().unwrap();
    assert_ne!(second.id, new.id);
    approve_and_assert_state(&mut storage, &second, &pubdata, deposit).await;
}

#[tokio::test]
async fn retained_completed_proof_remining_keeps_its_identity_and_verdict() {
    let (mut storage, work, _, pubdata) = approval_fixture(false, false).await;
    record_approval(&mut storage, &work, Approval::Verified, &pubdata, None).await.unwrap();
    // Retained inputs entering reset name their old, now noncanonical inclusions.
    let mut retained = work.proof.clone();
    retained.origin.height = 9;
    retained.origin.hash = "09".repeat(32).parse().unwrap();
    retained.payload.input.pubdata.origin.height = 8;
    retained.payload.input.pubdata.origin.hash = "08".repeat(32).parse().unwrap();
    sqlx::query("UPDATE via_votable_transactions SET bitcoin_payload=$2,proof_header_id=NULL,
        proof_l1_block_number=9,proof_l1_block_hash=$3,source_l1_block_number=9,source_l1_block_hash=$3 WHERE id=$1")
        .bind(work.id).bind(serde_json::to_value(&retained).unwrap()).bind(retained.origin.hash.to_string())
        .execute(storage.conn()).await.unwrap();
    let mut tx = storage.start_transaction().await.unwrap();
    tx.via_votes_dal().save_reader_proof(&work.proof, work.header_id).await.unwrap();
    tx.commit().await.unwrap();
    let rows: Vec<(i64, Option<bool>, Option<i64>)> =
        sqlx::query_as("SELECT id,l1_batch_status,proof_header_id FROM via_votable_transactions")
            .fetch_all(storage.conn())
            .await
            .unwrap();
    assert_eq!(rows, vec![(work.id, Some(true), Some(work.header_id))]);
    let mut changed = work.proof.clone();
    changed.payload.signature.push(1);
    let mut tx = storage.start_transaction().await.unwrap();
    let error = tx.via_votes_dal().save_reader_proof(&changed, work.header_id).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<via_btc_client::indexer::replay::ReplayConflict>().unwrap().field,
        "signature"
    );
}

#[tokio::test]
async fn watermarks_come_from_the_database_and_follow_a_reorg() {
    let (mut storage, tx_id, _, pubdata) = approval_fixture(false, false).await;
    // Fresh process sees pending batch without having processed anything.
    assert_eq!(report_watermarks(&mut storage).await.unwrap(), (1, 0));

    record_approval(&mut storage, &tx_id, Approval::Verified, &pubdata, None).await.unwrap();
    assert_eq!(report_watermarks(&mut storage).await.unwrap(), (1, 1));

    // Reorg removes batch, and both watermarks fall back with it.
    sqlx::query("DELETE FROM via_votable_transactions WHERE l1_batch_number = $1")
        .bind(BATCH)
        .execute(storage.conn())
        .await
        .unwrap();
    assert_eq!(report_watermarks(&mut storage).await.unwrap(), (0, 0));
}

/// Only the RPC transport and DA/object storage are doubled.
/// The inscription parser, proof verifier, selection, deposit checks and transaction writes remain production code.
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
    rpc: BitcoinRpcServer,
    client: Arc<BitcoinClient>,
    endpoint: String,
}

async fn lifecycle_fixture(dev_mode: bool, mismatch: bool) -> LifecycleFixture {
    lifecycle_fixture_with_genesis(dev_mode, mismatch, bitcoin::Network::Regtest).await
}

async fn lifecycle_fixture_with_genesis(
    dev_mode: bool,
    mismatch: bool,
    genesis_network: bitcoin::Network,
) -> LifecycleFixture {
    use via_btc_client::{
        inscriber::test_utils::{get_mock_inscriber_and_conditions, MockBitcoinOps},
        types::{InscriptionMessage, NodeAuth, ProofDAReferenceInput},
    };

    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let (mut storage, deposit, pubdata) = approval_store(pool.clone(), dev_mode, mismatch).await;
    let mut package = v28();
    package.proofs.clear();
    let batch = package.l1_batches[0].header.number.0;
    let mut inscriber = get_mock_inscriber_and_conditions(MockBitcoinOps::default());
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
    let work = materialize_work(
        &mut storage,
        proof_tx.compute_txid(),
        input.clone(),
        ProofDAReferenceInput {
            l1_batch_reveal_txid: batch_tx.compute_txid(),
            da_identifier: "fixture".into(),
            blob_id: "proof".into(),
        },
        wallets.clone(),
    )
    .await;
    let mut parent = input.clone();
    parent.l1_batch_index = zksync_types::L1BatchNumber(batch - 1);
    parent.l1_batch_hash = input.prev_l1_batch_hash;
    parent.prev_l1_batch_hash = H256::zero();
    let previous = materialize_work(
        &mut storage,
        "08".repeat(32).parse().unwrap(),
        parent,
        ProofDAReferenceInput {
            l1_batch_reveal_txid: "18".repeat(32).parse().unwrap(),
            da_identifier: "fixture".into(),
            blob_id: "previous".into(),
        },
        wallets.clone(),
    )
    .await;
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status=TRUE,is_finalized=TRUE,rotation_legacy_cutoff=1 WHERE id=$1")
        .bind(previous.id).execute(storage.conn()).await.unwrap();
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
    let vote = Included {
        origin: MessageOrigin {
            txid: "03".repeat(32).parse().unwrap(),
            transaction_index: 2,
            ..work.proof.origin.clone()
        },
        payload: Payload {
            input: via_btc_client::types::ValidatorAttestationInput {
                reference_txid: work.proof.origin.txid,
                attestation: via_btc_client::types::Vote::Ok,
            },
            public_key: vec![],
            signature: vec![],
            sender_script: Some(address.script_pubkey().into_bytes()),
        },
    };
    let mut tx = storage.start_transaction().await.unwrap();
    ReaderDal::new(&mut tx)
        .save_reader_vote(&vote, &work, bitcoin::Network::Regtest)
        .await
        .unwrap();
    tx.commit().await.unwrap();
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
    sqlx::query("UPDATE protocol_versions SET bitcoin_height=0,bitcoin_hash=$1; ")
        .bind(Chain::with_height(1).blocks[&0].block_hash().to_string())
        .execute(storage.conn())
        .await
        .unwrap();
    sqlx::query("UPDATE protocol_patches SET bitcoin_height=0,bitcoin_hash=$1")
        .bind(Chain::with_height(1).blocks[&0].block_hash().to_string())
        .execute(storage.conn())
        .await
        .unwrap();
    drop(storage);
    let mut chain = Chain::with_height(1);
    chain.blocks.insert(0, bitcoin::blockdata::constants::genesis_block(genesis_network));
    chain.transactions.insert(batch_tx.compute_txid().to_string(), batch_tx);
    chain.transactions.insert(proof_tx.compute_txid().to_string(), proof_tx);
    let rpc = BitcoinRpcServer::new_with_errors(None, false, move |_, request| chain.rpc(&request))
        .unwrap();
    let endpoint = rpc.url("");
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
    let mut config = ViaVerifierConfig::for_tests();
    config.wallet_address = address.to_string();
    config.proof_verification_dev_mode = dev_mode;
    let store = MockObjectStore::arc();
    let verifier = ViaVerifier::new(
        config,
        pool,
        Box::new(FixtureDa {
            proof: bincode::serialize(&package).unwrap(),
            pubdata: pubdata.encode_pubdata(),
        }),
        client.clone(),
        store.clone(),
    )
    .await
    .unwrap();
    LifecycleFixture { verifier, store, deposit, rpc, client, endpoint }
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

fn source_payload<T>(input: T) -> Payload<T> {
    Payload { input, public_key: vec![], signature: vec![], sender_script: None }
}

async fn decide_rotation_candidate(
    storage: &mut Connection<'_, Verifier>,
    id: i64,
) -> Option<bool> {
    let proofs = <Verifier as via_btc_storage::ReaderRole>::canonical_proofs(
        storage,
        1,
        via_btc_storage::ProofScope::Interpretation,
    )
    .await
    .unwrap();
    let proof_ids: Vec<_> = proofs.iter().map(|proof| proof.work.id).collect();
    let votes = ReaderDal::new(storage).reader_votes(1, &proof_ids).await.unwrap();
    let mut tx = storage.start_transaction().await.unwrap();
    let mut reader = ReaderDal::new(&mut tx);
    reader
        .decide_configurations(1, &proofs, &votes, true, bitcoin::Network::Regtest, 0.66)
        .await
        .unwrap();
    let accepted = reader
        .configurations_at(1)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == id)
        .unwrap()
        .accepted;
    tx.commit().await.unwrap();
    accepted
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
    let old_wallets = ReaderDal::new(&mut storage).wallets_before(1).await.unwrap().unwrap();
    let proposed = via_test_utils::utils::test_wallets();
    let proof = storage.via_votes_dal().pending_reader_proof().await.unwrap().unwrap();
    let origin = MessageOrigin {
        transaction_index: 3,
        txid: "05".repeat(32).parse().unwrap(),
        ..proof.proof.origin.clone()
    };
    let proposal = Included {
        origin: MessageOrigin {
            transaction_index: 0,
            txid: "15".repeat(32).parse().unwrap(),
            ..origin.clone()
        },
        payload: source_payload(via_btc_client::types::UpdateBridgeProposalInput {
            bridge_musig2_address: proposed.bridge.as_unchecked().clone(),
            verifier_p2wpkh_addresses: proposed
                .verifiers
                .iter()
                .map(|address| address.as_unchecked().clone())
                .collect(),
        }),
    };
    let change = Included {
        origin,
        payload: source_payload(
            via_btc_client::indexer::configuration::ConfigurationInput::Bridge(
                via_btc_client::types::UpdateBridgeInput {
                    inputs: vec![],
                    proposal_tx_id: proposal.origin.txid,
                },
                Some(Box::new(proposal)),
            ),
        ),
    };
    let mut tx = storage.start_transaction().await.unwrap();
    let rotation = ReaderDal::new(&mut tx).save_configuration(&change).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(decide_rotation_candidate(&mut storage, rotation).await, None);
    assert!(ReaderDal::new(&mut storage).wallets_before(2).await.unwrap().is_none());
    let before = durable_effects(&mut storage).await;
    let result = fixture.verifier.loop_iteration(&mut storage).await;
    assert_eq!(reason(result.map(|_| Approval::Verified)), NoVerdictReason::ProofUnavailable);
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
    // Reconnect as on restart: pending work comes from database, not memory.
    drop(storage);
    let mut storage = fixture.verifier.pool.connection().await.unwrap();
    assert_eq!(decide_rotation_candidate(&mut storage, rotation).await, None);
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
    assert_eq!(lifecycle_state(&fixture).await, (Some(true), false, Some(true), 1));
    assert_eq!(decide_rotation_candidate(&mut storage, rotation).await, Some(true));
    assert_eq!(ReaderDal::new(&mut storage).wallets_before(1).await.unwrap(), Some(old_wallets));
    let activated = ReaderDal::new(&mut storage).wallets_before(2).await.unwrap().unwrap();
    assert_eq!(activated.bridge, proposed.bridge);
    assert_eq!(activated.verifiers, proposed.verifiers);
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
    assert_eq!(recorded_vote_status(&mut storage).await, Some(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn development_absence_does_not_bypass_deposit_mismatch() {
    let mut fixture = lifecycle_fixture(true, true).await;
    let mut storage = fixture.verifier.pool.connection().await.unwrap();
    let before = durable_effects(&mut storage).await;
    let result = fixture.verifier.loop_iteration(&mut storage).await;
    assert_eq!(reason(result.map(|_| Approval::Verified)), NoVerdictReason::DepositMismatch);
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
        state.push(sqlx::query_scalar(&query).fetch_one(storage.conn()).await.unwrap());
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
    sqlx::query("INSERT INTO via_votes(votable_transaction_id,verifier_address,vote) VALUES(1,'verifier',TRUE)")
        .execute(storage.conn()).await.unwrap();
    sqlx::query(
        "ALTER TABLE protocol_versions ADD CONSTRAINT fixture_upgrade_failure CHECK (NOT executed)",
    )
    .execute(storage.conn())
    .await
    .unwrap();
    let before = durable_effects(&mut storage).await;
    record_approval(&mut storage, &tx_id, Approval::DevAccepted, &pubdata, Some(upgrade))
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
        &tx_id,
        Approval::DevAccepted,
        &pubdata,
        Some(upgrade),
    )
    .await
    .unwrap());
    assert_eq!(persisted(&mut storage, deposit).await, (Some(true), true, Some(true)));
    let finalized: Option<bool> =
        sqlx::query_scalar("SELECT is_finalized FROM via_votable_transactions WHERE id = 1")
            .fetch_one(storage.conn())
            .await
            .unwrap();
    assert_eq!(finalized, None);
    assert_eq!(
        storage.via_protocol_versions_dal().get_in_progress_upgrade_tx_hash().await.unwrap(),
        None
    );
}

#[tokio::test]
async fn stopped_reader_rolls_back_strict_and_development_approval_effects() {
    for (dev, approval) in [(false, Approval::Verified), (true, Approval::DevAccepted)] {
        let (mut storage, work, deposit, pubdata) = approval_fixture(dev, false).await;
        let mut stop = storage.start_transaction().await.unwrap();
        via_btc_storage::ReaderDal::new(&mut stop)
            .stop("fixture source invalidation")
            .await
            .unwrap();
        stop.commit().await.unwrap();
        let before = durable_effects(&mut storage).await;
        assert!(!record_approval(&mut storage, &work, approval, &pubdata, None).await.unwrap());
        assert_eq!(persisted(&mut storage, deposit).await, (None, false, None));
        assert_eq!(durable_effects(&mut storage).await, before);
    }
}

fn start_verifier(
    verifier: ViaVerifier,
) -> (watch::Sender<bool>, impl Future<Output = anyhow::Result<()>>) {
    let (stop, receiver) = watch::channel(false);
    (stop, verifier.run(receiver))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_interrupts_a_yielding_proof_fetch_without_a_verdict() {
    let mut fixture = lifecycle_fixture(false, false).await;
    let mut observer = fixture.verifier.pool.connection().await.unwrap();
    let before = durable_effects(&mut observer).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    fixture.verifier.blob_store = Arc::new(TestProofStore::Waiting(entered.clone()));
    let (stop, run) = start_verifier(fixture.verifier);
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
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut run).await.unwrap().unwrap();
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
    let (stop, run) = start_verifier(fixture.verifier);
    tokio::pin!(run);
    std::future::poll_fn(|cx| {
        assert!(run.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    // Register this later timer only after run has registered its first tick.
    // Poll run first when it expires, letting earlier tick reach exhausted pool before stop.
    tokio::select! {
        biased;
        result = &mut run => panic!("verifier exited while the pool was exhausted: {result:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
    }
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut run).await.unwrap().unwrap();
    assert_eq!(durable_effects(&mut held).await, before);
    drop(fixture.rpc);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_during_a_post_marker_database_wait_rolls_back_all_effects() {
    let fixture = lifecycle_fixture(true, false).await;
    let pool = fixture.verifier.pool.clone();
    let mut observer = pool.connection().await.unwrap();
    let mut blocker = pool.connection().await.unwrap();
    let blocker_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(blocker.conn()).await.unwrap();
    let before = durable_effects(&mut observer).await;
    let mut lock = blocker.start_transaction().await.unwrap();
    sqlx::query("SELECT canonical_tx_hash FROM via_transactions FOR UPDATE")
        .fetch_all(lock.conn())
        .await
        .unwrap();
    let (stop, run) = start_verifier(fixture.verifier);
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
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut run).await.unwrap().unwrap();
    lock.commit().await.unwrap();
    // Wait for cancelled write and queued rollback to release their row lock.
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
        dev_mode,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialized_development_store_reopens_but_refuses_a_strict_process() {
    use via_test_utils::bitcoin_core::BitcoinCore;
    let core = BitcoinCore::new().unwrap();
    let client = core.inscriber_client().unwrap();
    let protocol = zksync_types::ProtocolVersion {
        version: ProtocolSemanticVersion::new(ProtocolVersionId::Version27, VersionPatch(0)),
        base_system_contracts_hashes: zksync_contracts::BaseSystemContractsHashes {
            bootloader: H256::repeat_byte(1),
            default_aa: H256::repeat_byte(2),
            evm_emulator: Some(H256::repeat_byte(4)),
        },
        l1_verifier_config: zksync_types::protocol_version::L1VerifierConfig {
            snark_wrapper_vk_hash: H256::repeat_byte(3),
        },
        ..Default::default()
    };
    let bootstrap =
        core.bootstrap(&protocol, &via_test_utils::utils::test_wallets()).await.unwrap();
    let genesis = zksync_config::configs::via_consensus::ViaGenesisConfig {
        bootstrap_txids: vec![bootstrap.to_string()],
    };
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    via_verifier_storage_init::ViaVerifierStorageInitializer::new(
        pool.clone(),
        client.clone(),
        genesis.clone(),
        true,
    )
    .await
    .unwrap();
    let mut storage = pool.connection().await.unwrap();
    let before = ReaderDal::new(&mut storage).state().await.unwrap().unwrap();
    assert_eq!(before.bootstrap.height, 1);
    assert!(!before.replay_ready);
    drop(storage);
    via_verifier_storage_init::ViaVerifierStorageInitializer::new(
        pool.clone(),
        client.clone(),
        genesis.clone(),
        true,
    )
    .await
    .unwrap();
    assert!(via_verifier_storage_init::ViaVerifierStorageInitializer::new(
        pool.clone(),
        client,
        genesis,
        false
    )
    .await
    .is_err());
    let mut storage = pool.connection().await.unwrap();
    let after = ReaderDal::new(&mut storage).state().await.unwrap().unwrap();
    assert_eq!(
        (after.bootstrap, after.cutoff, after.confirmed, after.replay_ready),
        (before.bootstrap, before.cutoff, before.confirmed, before.replay_ready)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initializer_checks_actual_genesis_and_network_before_designating_the_store() {
    let fixture = lifecycle_fixture_with_genesis(false, false, bitcoin::Network::Bitcoin).await;
    for network in ["regtest", "malformed-network", "bitcoin"] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut config = fixture.client.config.clone();
        config.network = network.into();
        match BitcoinClient::new(&fixture.endpoint, via_btc_client::types::NodeAuth::None, config) {
            Ok(client) => assert!(initialize(pool.clone(),Arc::new(client),true).await.is_err()),
            Err(error) => assert!(matches!(error,via_btc_client::types::BitcoinError::InvalidNetwork(_))),
        }
        let mut storage = pool.connection().await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM via_verifier_store_mode")
            .fetch_one(storage.conn())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
    // Configured regtest endpoint serves Bitcoin genesis, and initialized store gets no exemption.
    assert!(initialize(fixture.verifier.pool.clone(), fixture.client.clone(), false)
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_bootstrap_keeps_the_committed_strict_designation() {
    let fixture = lifecycle_fixture(false, false).await;
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    initialize(pool.clone(), fixture.client.clone(), false).await.unwrap_err();
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
    assert!(initialize(pool, fixture.client.clone(), true).await.is_err());
}

async fn recorded_vote_status(storage: &mut Connection<'_, Verifier>) -> Option<bool> {
    storage.via_votes_dal().get_verifier_vote_status(1).await.unwrap().map(|row| row.0)
}
