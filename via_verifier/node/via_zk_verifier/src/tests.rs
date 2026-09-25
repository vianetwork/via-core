//! Outcomes of `verify_batch_proof` for genuine v27 and v28 proof packages, and of `record_approval` in a database.

use via_btc_client::types::L1BatchDAReferenceInput;
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
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    storage
        .via_store_mode_dal()
        .ensure_proof_verification_mode(&StoreMode {
            proof_verification_dev_mode: dev_mode,
            bitcoin_network: "regtest".into(),
            bitcoin_genesis_hash: "g".into(),
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
        .insert_transaction(0, H256::random(), "receiver".into(), 1, vec![], deposit, 1)
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
