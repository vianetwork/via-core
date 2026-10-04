//! Proof outcomes against genuine v27 and v28 proof packages.

use via_verification::{
    verify_proof, version_27::types::ProveBatches as ProveBatchesV27,
    version_28::types::ProveBatches as ProveBatchesV28, ProveBatchData,
};
use zksync_types::H256;

fn fixture(name: &str) -> Vec<u8> {
    std::env::set_var(
        "VIA_VK_KEY_PATH",
        concat!(env!("CARGO_MANIFEST_DIR"), "/keys"),
    );
    std::fs::read(format!(
        "{}/examples/data/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn v27() -> ProveBatchesV27 {
    bincode::deserialize(&fixture("data_batch_18_0_27_0.bin")).unwrap()
}

fn v28() -> ProveBatchesV28 {
    bincode::deserialize(&fixture("data_batch_9_0_28_0.bin")).unwrap()
}

/// Applies the same mutation to both versions and returns both outcomes.
async fn both(
    mutate27: impl FnOnce(&mut ProveBatchesV27),
    mutate28: impl FnOnce(&mut ProveBatchesV28),
) -> [anyhow::Result<bool>; 2] {
    let (mut a, mut b) = (v27(), v28());
    mutate27(&mut a);
    mutate28(&mut b);
    [
        verify_proof(ProveBatchData::V27(a)).await,
        verify_proof(ProveBatchData::V28(b)).await,
    ]
}

#[tokio::test]
async fn genuine_proof_verifies() {
    for outcome in both(|_| {}, |_| {}).await {
        assert!(outcome.unwrap());
    }
}

#[tokio::test]
async fn skip_flag_does_not_change_a_real_check() {
    let outcomes = both(|d| d.should_verify = false, |d| d.should_verify = false).await;
    for outcome in outcomes {
        assert!(outcome.unwrap());
    }
    let changed = H256::repeat_byte(7);
    let outcomes = both(
        |d| {
            d.should_verify = false;
            d.l1_batches[0].metadata.commitment = changed;
        },
        |d| {
            d.should_verify = false;
            d.l1_batches[0].metadata.commitment = changed;
        },
    )
    .await;
    for outcome in outcomes {
        assert!(!outcome.unwrap());
    }
}

#[tokio::test]
async fn changed_commitment_is_a_completed_negative() {
    let changed = H256::repeat_byte(7);
    let outcomes = both(
        |d| d.l1_batches[0].metadata.commitment = changed,
        |d| d.l1_batches[0].metadata.commitment = changed,
    )
    .await;
    for outcome in outcomes {
        assert!(!outcome.unwrap());
    }
}

#[tokio::test]
async fn malformed_packages_are_not_verdicts() {
    for outcome in both(|d| d.l1_batches.clear(), |d| d.l1_batches.clear()).await {
        assert!(outcome.is_err());
    }
    let outcomes = both(
        |d| d.l1_batches.push(d.l1_batches[0].clone()),
        |d| d.l1_batches.push(d.l1_batches[0].clone()),
    )
    .await;
    for outcome in outcomes {
        assert!(outcome.is_err());
    }
    for outcome in both(|d| d.proofs.clear(), |d| d.proofs.clear()).await {
        assert!(outcome.is_err());
    }
    let outcomes = both(
        |d| d.proofs.push(d.proofs[0].clone()),
        |d| d.proofs.push(d.proofs[0].clone()),
    )
    .await;
    for outcome in outcomes {
        assert!(outcome.is_err());
    }
}

#[test]
fn shape_allows_one_missing_proof_only() {
    let mut data = v28();
    data.proofs.clear();
    let missing = ProveBatchData::V28(data.clone());
    assert!(missing.check_shape().is_ok());
    assert!(!missing.has_proof());

    data.l1_batches.clear();
    assert!(ProveBatchData::V28(data).check_shape().is_err());

    let mut doubled = v27();
    doubled.proofs.push(doubled.proofs[0].clone());
    assert!(ProveBatchData::V27(doubled).check_shape().is_err());
}
