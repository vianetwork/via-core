//! Cross-engine byte-identity check: two independent [`ProtocolEngine`]
//! implementations must produce byte-identical canonical plans for the
//! same envelope, context, and dependency answers. Until a second engine
//! is merged, the reference engine is compared against itself, which keeps
//! the corpus and driver compiled and honest.

use std::collections::BTreeMap;

use bitcoin::Network;
use via_btc_client::{ingestion_engine::ViaProtocolEngine, test_message_encoder::*};
use via_btc_ingestion::{
    BitcoinBlockEnvelope, BlockAnchor, DependencyKey, FinalizeFailure, FinalizeOutcome,
    ProtocolContext, ProtocolEngine, ProtocolVersionTag, Resolution, ResolvedDependency,
};

const MAX_ROUNDS: usize = 4;

fn all_known_absent(keys: &[DependencyKey]) -> BTreeMap<DependencyKey, ResolvedDependency> {
    keys.iter()
        .cloned()
        .map(|k| {
            let v = match &k {
                DependencyKey::RawTx(_) => ResolvedDependency::RawTx(Resolution::KnownAbsent),
                DependencyKey::TrackedOutput(_) => {
                    ResolvedDependency::TrackedOutput(Resolution::KnownAbsent)
                }
            };
            (k, v)
        })
        .collect()
}

/// Drive one engine to a terminal outcome with every dependency answered
/// KnownAbsent. Returns the canonical plan bytes, or a stable string for
/// halts so failure behavior is compared too.
fn run<E: ProtocolEngine>(
    engine: &E,
    env: &BitcoinBlockEnvelope,
    ctx: &ProtocolContext,
) -> Vec<u8> {
    let (mut draft, keys) = engine.inspect(env, ctx);
    let mut resolved = all_known_absent(&keys);
    for _ in 0..MAX_ROUNDS {
        match engine.finalize(draft, &resolved) {
            Ok(FinalizeOutcome::Complete(plan)) => {
                return plan
                    .canonical_bytes()
                    .expect("complete plans have canonical form");
            }
            Ok(FinalizeOutcome::NeedDependencies { draft: d, keys }) => {
                resolved.extend(all_known_absent(&keys));
                draft = d;
            }
            Err(FinalizeFailure::MissingDependency(keys)) => {
                return format!("halt:missing:{keys:?}").into_bytes();
            }
            Err(e) => return format!("halt:{e}").into_bytes(),
        }
    }
    panic!("engine did not converge within {MAX_ROUNDS} rounds");
}

fn corpus() -> Vec<(String, BitcoinBlockEnvelope, ProtocolContext)> {
    let enc = TestMessageEncoder::new(Network::Regtest);
    let wallets = enc.wallet_set();
    let ctx = ProtocolContext {
        version: 1,
        wallets: wallets.clone(),
        protocol_version: ProtocolVersionTag {
            minor: 26,
            patch: 0,
        },
    };
    let unbootstrapped = ProtocolContext {
        version: 1,
        wallets: via_btc_ingestion::WalletSet {
            sequencer: bitcoin::ScriptBuf::new(),
            bridge: bitcoin::ScriptBuf::new(),
            governance: bitcoin::ScriptBuf::new(),
            verifiers: vec![],
        },
        protocol_version: ProtocolVersionTag { minor: 0, patch: 0 },
    };
    let anchor = |height: u64| BlockAnchor {
        height,
        hash: bitcoin::BlockHash::from_byte_array([height as u8; 32]),
        prev_hash: bitcoin::BlockHash::from_byte_array([height.saturating_sub(1) as u8; 32]),
        time: 1_700_000_000 + height as u32,
    };
    let env = |height: u64, txs: Vec<bitcoin::Transaction>| BitcoinBlockEnvelope {
        network: Network::Regtest,
        anchor: anchor(height),
        transactions: txs,
    };
    let op = |n: u8| bitcoin::OutPoint {
        txid: bitcoin::Txid::from_byte_array([n; 32]),
        vout: 0,
    };

    let batch = enc.batch_da_reference_tx(7, [3; 32], "blob-batch", op(2));
    let batch_txid = batch.compute_txid();
    let proof = enc.proof_da_reference_tx(batch_txid, "blob-proof", op(3));
    let proof_txid = proof.compute_txid();

    vec![
        ("empty".into(), env(100, vec![]), ctx.clone()),
        (
            "bootstrap-on-unbootstrapped".into(),
            env(
                100,
                vec![enc.bootstrap_tx(
                    &wallets,
                    ProtocolVersionTag {
                        minor: 26,
                        patch: 0,
                    },
                    op(1),
                )],
            ),
            unbootstrapped,
        ),
        (
            "batch-ref".into(),
            env(101, vec![batch.clone()]),
            ctx.clone(),
        ),
        (
            "same-block-batch-proof-attestation".into(),
            env(
                102,
                vec![batch, proof, enc.attestation_tx(proof_txid, true, 0, op(4))],
            ),
            ctx.clone(),
        ),
        (
            "unauthorized-attestation".into(),
            env(
                103,
                vec![
                    enc.unauthorized_attestation_tx(bitcoin::Txid::from_byte_array([9; 32]), op(5))
                ],
            ),
            ctx.clone(),
        ),
        (
            "proposal-and-unresolvable-activation".into(),
            env(
                104,
                vec![
                    enc.upgrade_proposal_tx(
                        ProtocolVersionTag {
                            minor: 27,
                            patch: 0,
                        },
                        op(6),
                    ),
                    enc.upgrade_activation_tx(bitcoin::Txid::from_byte_array([8; 32]), op(7)),
                ],
            ),
            ctx.clone(),
        ),
        (
            "rotation-unauthorized".into(),
            env(
                105,
                vec![enc.sequencer_rotation_tx(&wallets.governance, op(8))],
            ),
            ctx.clone(),
        ),
        (
            "withdrawal-unauthorized".into(),
            env(
                106,
                vec![enc.withdrawal_tx(&[(wallets.governance.clone(), 1_000, [7u8; 8])], op(9))],
            ),
            ctx,
        ),
    ]
}

use bitcoin::hashes::Hash;

#[test]
fn reference_engine_agrees_with_itself_over_the_corpus() {
    let a = ViaProtocolEngine::new(Network::Regtest);
    let b = ViaProtocolEngine::new(Network::Regtest);
    for (name, env, ctx) in corpus() {
        let left = run(&a, &env, &ctx);
        let right = run(&b, &env, &ctx);
        assert_eq!(left, right, "case {name}: two runs must be byte-identical");
    }
}
