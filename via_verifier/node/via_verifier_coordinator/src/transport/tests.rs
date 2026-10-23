use bitcoin::{absolute, transaction, Amount, Transaction};
use via_verifier_types::transaction::UnsignedBridgeTx;

use super::*;
use crate::{auth::digest, types::AdmittedContent, utils::encode_signature};

fn session(inputs: usize) -> SigningSession {
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: Vec::new(),
        output: Vec::new(),
    };
    let operation = SessionOperation::Withdrawal(
        UnsignedBridgeTx {
            txid: tx.compute_txid(),
            tx,
            utxos: Vec::new(),
            change_amount: Amount::ZERO,
            fee: Amount::ZERO,
            fee_rate: 1,
        },
        vec![vec![1; 32]; inputs],
        Vec::new(),
    );
    let authorized = bincode::serialize(&AdmittedContent {
        proposal: operation.to_bytes(),
        network: bitcoin::Network::Regtest,
        chain_id: 270,
        wallet: vec![1],
        participants: vec!["test".into()],
        tweak: None,
        expected: Vec::new(),
    })
    .unwrap();
    SigningSession {
        round_id: [1; 32],
        content_hash: digest(&authorized),
        authorized_content: authorized,
        session_op: Some(operation),
        created_at: 1,
        ..Default::default()
    }
}

fn binding() -> Binding {
    Binding {
        version: 1,
        sequencer_version: "0.1.0".into(),
        principal: "02".repeat(33),
        audience: "03".repeat(33),
        method: "GET".into(),
        target: "/session/".into(),
        round: [1; 32],
        content: [2; 32],
        challenge: [3; 32],
        timestamp: 1,
        status: 0,
    }
}

#[test]
fn bounded_signing_complete_transcript_matches_actual_json() {
    let key = bitcoin::PrivateKey::new(
        bitcoin::secp256k1::SecretKey::from_slice(&[1; 32]).unwrap(),
        bitcoin::Network::Regtest,
    );
    let public = key.public_key(&bitcoin::secp256k1::Secp256k1::new()).to_string();
    let mut signer =
        via_musig2::get_signer_with_merkle_root(&key.to_wif(), vec![public], None).unwrap();
    let nonce = encode_nonce(0, signer.start_signing_session(vec![7; 32]).unwrap()).unwrap().nonce;
    let signature = encode_signature(0, musig2::PartialSignature::from_slice(&[1; 32]).unwrap())
        .unwrap()
        .signature;
    assert_eq!(nonce.len(), 88);
    assert_eq!(signature.len(), 44);
    for inputs in [1, 9, 10, 11, 99, 100, 101, 1_740] {
        for signers in [1, 2, 9, 10, 11] {
            let session = session(inputs);
            let predicted = future_inner_bytes(&session, signers).unwrap();
            let mut response = empty_snapshot(&session, signers);
            for input in 0..inputs {
                response
                    .nonces
                    .insert(input, (0..signers).map(|signer| (signer, nonce.clone())).collect());
                response.signatures.insert(
                    input,
                    (0..signers).map(|signer| (signer, signature.clone())).collect(),
                );
                response.received_nonces.insert(input, signers);
                response.received_partial_signatures.insert(input, signers);
            }
            assert_eq!(predicted, serde_json::to_vec(&response).unwrap().len());
        }
    }
    assert!(matrix_size(usize::MAX, usize::MAX, 88).is_none());
    assert!(check_round_size(&session(1), usize::MAX).is_err());
}

#[test]
fn bounded_signing_final_writer_accepts_equality_and_refuses_one_extra_byte() {
    for len in [MAX_BODY - 1, MAX_BODY] {
        assert_eq!(bounded_json(&"a".repeat(len - 2), MAX_BODY).unwrap().len(), len);
    }
    assert!(bounded_json(&"a".repeat(MAX_BODY - 1), MAX_BODY).is_err());
}

#[test]
fn bounded_signing_metadata_reserves_worst_response_before_acceptance() {
    let mut binding = binding();
    check_metadata(&binding, &binding.audience).unwrap();
    binding.target = "\"".repeat(MAX_ENVELOPE_OVERHEAD);
    assert!(check_metadata(&binding, &binding.audience).is_err());
}

#[test]
fn bounded_signing_full_future_can_fail_when_empty_snapshot_fits() {
    let session = session(1_740);
    let empty = serde_json::to_vec(&empty_snapshot(&session, 20)).unwrap();
    assert!(MAX_ENVELOPE_OVERHEAD + 4 * empty.len() < MAX_BODY);
    assert!(check_round_size(&session, 20).is_err());
}

#[test]
fn bounded_signing_size_counter_has_bounded_exported_stage() {
    use crate::metrics::{SigningSizeLabel, SigningSizeStage, METRICS};
    METRICS.signing_size_refusals[&SigningSizeLabel { stage: SigningSizeStage::Admission }].inc();
    let mut registry = vise::Registry::empty();
    registry.register_metrics(&*METRICS);
    let mut text = String::new();
    registry.encode(&mut text, vise::Format::Prometheus).unwrap();
    assert!(
        text.contains("via_verifier_coordinator_signing_size_refusals{stage=\"admission\"}"),
        "{text}"
    );
}
