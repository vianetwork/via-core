use bitcoin::{absolute, transaction, Amount, Transaction};
use via_btc_client::traits::Serializable;
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
            for version in [TransportVersion::Legacy, TransportVersion::Compact] {
                let session = session(inputs);
                let predicted = future_inner_bytes(&session, signers, version).unwrap();
                let mut response = empty_snapshot(&session, signers, version).unwrap();
                for input in 0..inputs {
                    response.nonces.insert(
                        input,
                        (0..signers).map(|signer| (signer, nonce.clone())).collect(),
                    );
                    response.signatures.insert(
                        input,
                        (0..signers).map(|signer| (signer, signature.clone())).collect(),
                    );
                    response.received_nonces.insert(input, signers);
                    response.received_partial_signatures.insert(input, signers);
                }
                assert_eq!(predicted, encode_snapshot(response, version).unwrap().len());
            }
        }
    }
    assert!(matrix_size(usize::MAX, usize::MAX, 88).is_none());
    assert!(check_round_size(&session(1), usize::MAX).is_err());
}

#[test]
fn bounded_signing_size_counter_has_bounded_exported_stage() {
    use crate::metrics::{SigningSizeLabel, SigningSizeStage, METRICS};
    METRICS.signing_size_refusals[&SigningSizeLabel { stage: SigningSizeStage::Admission }].inc();
    let mut registry = vise::Registry::empty();
    registry.register_metrics(&*METRICS);
    let mut text = String::new();
    registry.encode(&mut text, vise::Format::OpenMetricsForPrometheus).unwrap();
    assert!(
        text.contains("via_verifier_coordinator_signing_size_refusals{stage=\"admission\"}"),
        "{text}"
    );
}

#[test]
fn bounded_signing_compact_http_preserves_auth_and_admitted_bytes() {
    use bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
    for version in [TransportVersion::Legacy, TransportVersion::Compact] {
        let key = SecretKey::from_slice(&[7; 32]).unwrap();
        let public = PublicKey::from_secret_key(&Secp256k1::new(), &key);
        let mut binding = binding();
        binding.principal = public.to_string();
        let envelope = Envelope::sign(binding.clone(), vec![0, 1, 255], &key).unwrap();
        let encoded = encode_envelope(envelope, version, MAX_BODY).unwrap();
        let (decoded, actual) = decode_envelope(&encoded).unwrap();
        assert_eq!(actual, version);
        assert_eq!(decoded.binding, binding);
        assert_eq!(decoded.body, [0, 1, 255]);
        decoded.verify(&public, 1, 30).unwrap();
        let session = session(2);
        let snapshot = empty_snapshot(&session, 2, version).unwrap();
        let body = encode_snapshot(snapshot, version).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value.get("session_op").is_none(), version == TransportVersion::Compact);
        let normalized = decode_snapshot(&body, version).unwrap();
        assert_eq!(normalized.authorized_content, session.authorized_content);
        assert_eq!(normalized.content_hash, session.content_hash);
        assert_eq!(normalized.session_op, session.session_op.as_ref().unwrap().to_bytes());
        if version == TransportVersion::Compact {
            let mut corrupted = value.clone();
            corrupted["authorized_content"] = serde_json::json!("AA==");
            let error =
                decode_snapshot(&serde_json::to_vec(&corrupted).unwrap(), version).unwrap_err();
            assert!(error.to_string().contains("Invalid proposal commitment"));
            corrupted = value.clone();
            corrupted["transport_version"] = serde_json::json!(1);
            assert!(decode_snapshot(&serde_json::to_vec(&corrupted).unwrap(), version).is_err());
        } else {
            let mut corrupted = value.clone();
            corrupted["session_op"] = serde_json::json!([0]);
            assert!(decode_snapshot(&serde_json::to_vec(&corrupted).unwrap(), version).is_err());
        }
    }
}

// serde_with's padding and alphabet cases, run at real HTTP boundary: https://github.com/jonasbb/serde_with/blob/8c59954436b139b238dc23830946f04a3e331862/serde_with/tests/base64.rs
#[test]
fn bounded_signing_base64_rejects_invalid_and_hybrid_fields() {
    let envelope =
        Envelope { binding: binding(), body: vec![0xe0, 0x7d], signature: "A".repeat(88) };
    let valid = encode_envelope(envelope, TransportVersion::Compact, MAX_BODY).unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    for accepted in ["4H0=", "4H0"] {
        json["body"] = serde_json::json!(accepted);
        assert_eq!(
            decode_envelope(&serde_json::to_vec(&json).unwrap()).unwrap().0.body,
            [0xe0, 0x7d]
        );
    }
    for rejected in ["0", "zz", "!", "AA===", "A==="] {
        json["body"] = serde_json::json!(rejected);
        assert!(decode_envelope(&serde_json::to_vec(&json).unwrap()).is_err());
    }
    json = serde_json::from_slice(&valid).unwrap();
    json["binding"]["round"] = serde_json::json!("AA==");
    assert!(decode_envelope(&serde_json::to_vec(&json).unwrap()).is_err());
    json = serde_json::from_slice(&valid).unwrap();
    json["binding"]["round"] = serde_json::json!(vec![0; 32]);
    assert!(decode_envelope(&serde_json::to_vec(&json).unwrap()).is_err());
    assert!(decode_envelope(&valid[..valid.len() - 1]).is_err());
}

// serde_json's borrowed RawValue whitespace and roundtrip cases, applied to envelope probe: https://github.com/serde-rs/json/blob/0903de449c52c1b4a2271e909b7afb18909dc379/tests/test.rs
#[test]
fn bounded_signing_borrowed_probe_preserves_legacy_body_whitespace() {
    let envelope =
        Envelope { binding: binding(), body: vec![0, 42, 255], signature: "A".repeat(88) };
    let encoded =
        String::from_utf8(encode_envelope(envelope, TransportVersion::Legacy, MAX_BODY).unwrap())
            .unwrap();
    let encoded = encoded.replace("[0,42,255]", "[ 0, 42, 255 ]");
    let (decoded, version) = decode_envelope(encoded.as_bytes()).unwrap();
    assert_eq!(version, TransportVersion::Legacy);
    assert_eq!(decoded.body, [0, 42, 255]);
    let duplicate = encoded.replace("\"body\":", "\"body\":[],\"body\":");
    assert!(decode_envelope(duplicate.as_bytes()).is_err());
    let unknown = encoded.replace("\"body\":", "\"transport_version\":2,\"body\":");
    assert!(decode_envelope(unknown.as_bytes()).is_err());
}

#[test]
fn bounded_signing_new_admission_keeps_legacy_capacity() {
    let session = session(1_740);
    assert!(check_round_size_in(&session, 20, TransportVersion::Compact).is_ok());
    assert!(check_round_size_in(&session, 100, TransportVersion::Compact).is_err());
    assert!(check_round_size_in(&session, 20, TransportVersion::Legacy).is_err());
    assert!(check_round_size(&session, 20).is_err());
}

#[test]
fn bounded_signing_binary_goldens_are_independent_of_http_adapters() {
    use base64::Engine;
    use bitcoin::secp256k1::{ecdsa::Signature, Message, PublicKey, Secp256k1, SecretKey};
    let expected_hash =
        hex::decode("4d8d9098fa1dd763efce1af1ac904ccdfee79a50700fc6f92ae8708ef0aab2c7").unwrap();
    let key = SecretKey::from_slice(&[7; 32]).unwrap();
    for version in [TransportVersion::Legacy, TransportVersion::Compact] {
        let envelope = Envelope::sign(binding(), vec![0, 1, 255], &key).unwrap();
        let (envelope, _) =
            decode_envelope(&encode_envelope(envelope, version, MAX_BODY).unwrap()).unwrap();
        let signature =
            base64::engine::general_purpose::STANDARD.decode(&envelope.signature).unwrap();
        Secp256k1::new()
            .verify_ecdsa(
                &Message::from_digest_slice(&expected_hash).unwrap(),
                &Signature::from_compact(&signature).unwrap(),
                &PublicKey::from_secret_key(&Secp256k1::new(), &key),
            )
            .unwrap();
    }
    // Built by hand from baseline bincode's little-endian field layout, not by new serializer.
    let bytes = hex::decode(concat!(
        "03000000000000000001ff040000000e01000000000000010000000000000051",
        "0100000000000000420000000000000030323032303230323032303230323032",
        "3032303230323032303230323032303230323032303230323032303230323032",
        "303230323032303230323032303230323032000000000000000000",
    ))
    .unwrap();
    let admitted: AdmittedContent = bincode::deserialize(&bytes).unwrap();
    assert_eq!(admitted.proposal, [0, 1, 255]);
    assert_eq!(admitted.network, bitcoin::Network::Regtest);
    assert_eq!(admitted.chain_id, 270);
    assert_eq!(admitted.wallet, [81]);
    assert_eq!(admitted.participants, ["02".repeat(33)]);
    assert!(admitted.tweak.is_none());
    assert!(admitted.expected.is_empty());
    assert_eq!(bincode::serialize(&admitted).unwrap(), bytes);
}

#[test]
fn bounded_signing_metadata_limit_includes_worst_width_and_escaped_fields() {
    let mut binding = binding();
    binding.target.push('"');
    binding.round = [255; 32];
    binding.content = [255; 32];
    binding.challenge = [255; 32];
    let worst = response_binding(&binding, &binding.audience, u16::MAX, i64::MIN);
    let overhead = serde_json::to_vec(&Envelope {
        binding: worst,
        body: Vec::new(),
        signature: "A".repeat(88),
    })
    .unwrap()
    .len();
    binding.target.extend(std::iter::repeat('x').take(MAX_ENVELOPE_OVERHEAD - overhead));
    check_metadata(&binding, &binding.audience).unwrap();
    binding.target.push('x');
    assert!(check_metadata(&binding, &binding.audience).is_err());
}
