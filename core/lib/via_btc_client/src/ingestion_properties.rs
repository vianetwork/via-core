use std::{
    any::Any,
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
};

use crate::{
    indexer::positioned::PositionedMessageParser,
    ingestion_engine::ViaProtocolEngine,
    test_message_encoder::{
        external_input, inscription_witness_for_script, keypair, op_return, p2tr, p2wpkh, push,
        rotation_tx, seeded_outpoint, transaction, BRIDGE_SEED, GOVERNANCE_SEED, SEQUENCER_SEED,
        VERIFIER_SEED,
    },
    types,
};
use bitcoin::{
    absolute::LockTime,
    hashes::Hash,
    key::UntweakedPublicKey,
    opcodes::{
        all::{OP_CHECKSIG, OP_ENDIF, OP_IF, OP_RETURN},
        OP_FALSE, OP_TRUE,
    },
    script::Builder,
    transaction::Version,
    Address, Amount, BlockHash, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
    Txid, Witness,
};
use rand::{rngs::StdRng, Rng, RngCore, SeedableRng};
use via_btc_ingestion::{
    BitcoinBlockEnvelope, BlockAnchor, DependencyKey, FinalizeOutcome, ProtocolContext,
    ProtocolEngine, ProtocolVersionTag, Resolution, ResolvedDependency, WalletSet,
};
use zksync_types::{
    ethabi::ethereum_types::BigEndianHash,
    protocol_version::{ProtocolSemanticVersion, ProtocolVersionId, VersionPatch},
    via_wallet::SystemWallets,
    H256,
};

const RANDOM_SEED: u64 = 0x5649_4150_524f_5031;
const MUTATION_SEED: u64 = 0x5649_4150_524f_5032;
const STRUCTURE_SEED: u64 = 0x5649_4150_524f_5033;

const RANDOM_COUNT: usize = 5_000;
const MUTATION_COUNT: usize = 4_000;
const STRUCTURE_COUNT: usize = 1_000;

#[derive(Clone, Copy, Debug)]
enum ContextKind {
    Bootstrapped,
    Unbootstrapped,
}

#[derive(Clone, Debug)]
struct CorpusCase {
    tx: Transaction,
    context: ContextKind,
    description: String,
}

#[derive(Clone, Copy, Debug)]
enum CarrierTarget {
    Witness { input: usize, item: usize },
    Output { index: usize },
}

#[derive(Clone, Debug)]
struct CarrierTemplate {
    name: &'static str,
    tx: Transaction,
    target: CarrierTarget,
    context: ContextKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EngineResult {
    Complete { canonical: Vec<u8>, hash: [u8; 32] },
    Failure(String),
    InvalidPlan(String),
    DependencyCap(Vec<DependencyKey>),
}

fn wallets() -> SystemWallets {
    SystemWallets {
        sequencer: p2wpkh(SEQUENCER_SEED, Network::Regtest).0,
        bridge: p2tr(BRIDGE_SEED, Network::Regtest),
        governance: p2wpkh(GOVERNANCE_SEED, Network::Regtest).0,
        verifiers: vec![p2wpkh(VERIFIER_SEED, Network::Regtest).0],
    }
}

fn protocol_context(kind: ContextKind) -> ProtocolContext {
    match kind {
        ContextKind::Bootstrapped => {
            let wallets = wallets();
            ProtocolContext {
                version: 1,
                wallets: WalletSet {
                    sequencer: wallets.sequencer.script_pubkey(),
                    bridge: wallets.bridge.script_pubkey(),
                    governance: wallets.governance.script_pubkey(),
                    verifiers: wallets
                        .verifiers
                        .iter()
                        .map(Address::script_pubkey)
                        .collect(),
                },
                protocol_version: ProtocolVersionTag {
                    minor: 26,
                    patch: 0,
                },
            }
        }
        ContextKind::Unbootstrapped => ProtocolContext {
            version: 1,
            wallets: WalletSet {
                sequencer: ScriptBuf::new(),
                bridge: ScriptBuf::new(),
                governance: ScriptBuf::new(),
                verifiers: vec![],
            },
            protocol_version: ProtocolVersionTag { minor: 0, patch: 0 },
        },
    }
}

fn base_inscription(internal_key: UntweakedPublicKey) -> Builder {
    Builder::new()
        .push_slice(internal_key.serialize())
        .push_opcode(OP_CHECKSIG)
        .push_opcode(OP_FALSE)
        .push_opcode(OP_IF)
        .push_slice(push(b"via_inscription_protocol"))
}

fn inscribed_transaction(script: ScriptBuf, signer_seed: u8, inscription_seed: u8) -> Transaction {
    let internal_key = keypair(inscription_seed).x_only_public_key().0;
    transaction(
        vec![
            external_input(
                seeded_outpoint(inscription_seed),
                p2wpkh(signer_seed, Network::Regtest).1,
            ),
            external_input(
                seeded_outpoint(inscription_seed.wrapping_add(1)),
                inscription_witness_for_script(script, internal_key),
            ),
        ],
        vec![],
    )
}

fn huge_op_return(size: usize, fill: u8) -> ScriptBuf {
    let size = size.min(u16::MAX as usize);
    let mut raw = Vec::with_capacity(size + 4);
    raw.push(OP_RETURN.to_u8());
    raw.push(0x4d); // OP_PUSHDATA2
    raw.extend_from_slice(&(size as u16).to_le_bytes());
    raw.extend(std::iter::repeat(fill).take(size));
    ScriptBuf::from_bytes(raw)
}

fn bridge_output(value: u64) -> TxOut {
    TxOut {
        value: Amount::from_sat(value),
        script_pubkey: wallets().bridge.script_pubkey(),
    }
}

fn carrier_templates() -> Vec<CarrierTemplate> {
    let bootstrap_key = keypair(31).x_only_public_key().0;
    let semver = ProtocolSemanticVersion::new(ProtocolVersionId::Version26, VersionPatch(0));
    let wallets = wallets();
    let bootstrap = base_inscription(bootstrap_key)
        .push_slice(&*types::SYSTEM_BOOTSTRAPPING_MSG)
        .push_slice(push(100_u32.to_be_bytes()))
        .push_slice(push(H256::from_uint(&semver.pack()).as_bytes()))
        .push_slice(push([1; 32]))
        .push_slice(push([2; 32]))
        .push_slice(push([3; 32]))
        .push_slice(push([4; 32]))
        .push_slice(push(wallets.governance.to_string()))
        .push_slice(push(wallets.sequencer.to_string()))
        .push_slice(push(wallets.bridge.to_string()))
        .push_slice(push(wallets.verifiers[0].to_string()))
        .push_opcode(OP_ENDIF)
        .into_script();
    let bootstrap_tx = inscribed_transaction(bootstrap, 41, 31);

    let deposit_key = keypair(32).x_only_public_key().0;
    let deposit = base_inscription(deposit_key)
        .push_slice(&*types::L1_TO_L2_MSG)
        .push_slice(push([0x51; 20]))
        .push_slice(push([0x61; 20]))
        .push_slice(push([1, 2, 3, 4]))
        .push_opcode(OP_ENDIF)
        .into_script();
    let mut deposit_tx = inscribed_transaction(deposit, 42, 32);
    deposit_tx.output.push(bridge_output(25_000));

    let batch_key = keypair(33).x_only_public_key().0;
    let batch = base_inscription(batch_key)
        .push_slice(&*types::L1_BATCH_DA_REFERENCE_MSG)
        .push_slice(push([0x71; 32]))
        .push_slice(push(7_u32.to_be_bytes()))
        .push_slice(push(b"celestia"))
        .push_slice(push(b"batch-blob"))
        .push_slice(push([0x70; 32]))
        .push_opcode(OP_ENDIF)
        .into_script();
    let batch_tx = inscribed_transaction(batch, SEQUENCER_SEED, 33);

    let proof_key = keypair(34).x_only_public_key().0;
    let proof = base_inscription(proof_key)
        .push_slice(&*types::PROOF_DA_REFERENCE_MSG)
        .push_slice(push([0x81; 32]))
        .push_slice(push(b"celestia"))
        .push_slice(push(b"proof-blob"))
        .push_opcode(OP_ENDIF)
        .into_script();
    let proof_tx = inscribed_transaction(proof, SEQUENCER_SEED, 34);

    let attestation_key = keypair(35).x_only_public_key().0;
    let attestation = base_inscription(attestation_key)
        .push_slice(&*types::VALIDATOR_ATTESTATION_MSG)
        .push_slice(push([0x91; 32]))
        .push_opcode(OP_TRUE)
        .push_opcode(OP_ENDIF)
        .into_script();
    let attestation_tx = inscribed_transaction(attestation, VERIFIER_SEED, 35);

    let mut withdrawal_payload = b"VIA_WI".to_vec();
    withdrawal_payload.push(0);
    withdrawal_payload.extend_from_slice(&[0xa1; 10]);
    let withdrawal_tx = transaction(
        vec![external_input(seeded_outpoint(36), Witness::new())],
        vec![
            TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: p2wpkh(44, Network::Regtest).0.script_pubkey(),
            },
            op_return(withdrawal_payload),
        ],
    );

    let sequencer_address = p2wpkh(45, Network::Regtest).0;
    let sequencer_rotation = rotation_tx(
        seeded_outpoint(37),
        b"VIA_PROTOCOL:SEQ",
        sequencer_address.to_string().as_bytes(),
    );
    let governance_address = p2wpkh(46, Network::Regtest).0;
    let governance_rotation = rotation_tx(
        seeded_outpoint(38),
        b"VIA_PROTOCOL:GOV",
        governance_address.to_string().as_bytes(),
    );

    vec![
        CarrierTemplate {
            name: "bootstrap",
            tx: bootstrap_tx,
            target: CarrierTarget::Witness { input: 1, item: 1 },
            context: ContextKind::Unbootstrapped,
        },
        CarrierTemplate {
            name: "deposit",
            tx: deposit_tx,
            target: CarrierTarget::Witness { input: 1, item: 1 },
            context: ContextKind::Bootstrapped,
        },
        CarrierTemplate {
            name: "batch-da-reference",
            tx: batch_tx,
            target: CarrierTarget::Witness { input: 1, item: 1 },
            context: ContextKind::Bootstrapped,
        },
        CarrierTemplate {
            name: "proof-da-reference",
            tx: proof_tx,
            target: CarrierTarget::Witness { input: 1, item: 1 },
            context: ContextKind::Bootstrapped,
        },
        CarrierTemplate {
            name: "validator-attestation",
            tx: attestation_tx,
            target: CarrierTarget::Witness { input: 1, item: 1 },
            context: ContextKind::Bootstrapped,
        },
        CarrierTemplate {
            name: "withdrawal",
            tx: withdrawal_tx,
            target: CarrierTarget::Output { index: 1 },
            context: ContextKind::Bootstrapped,
        },
        CarrierTemplate {
            name: "sequencer-rotation",
            tx: sequencer_rotation,
            target: CarrierTarget::Output { index: 0 },
            context: ContextKind::Bootstrapped,
        },
        CarrierTemplate {
            name: "governance-rotation",
            tx: governance_rotation,
            target: CarrierTarget::Output { index: 0 },
            context: ContextKind::Bootstrapped,
        },
    ]
}

fn mutate_bytes(bytes: &mut Vec<u8>, operation: usize, rng: &mut StdRng) {
    match operation {
        0 => {
            let new_len = rng.gen_range(0..=bytes.len());
            bytes.truncate(new_len);
        }
        1 => {
            if bytes.is_empty() {
                bytes.push(rng.gen());
            } else {
                let index = rng.gen_range(0..bytes.len());
                bytes[index] ^= 1_u8 << rng.gen_range(0..8);
            }
        }
        2 => {
            let extra_len = rng.gen_range(521..=1_024);
            let old_len = bytes.len();
            bytes.resize(old_len + extra_len, 0);
            rng.fill_bytes(&mut bytes[old_len..]);
        }
        3 => {
            let index = rng.gen_range(0..=bytes.len());
            bytes.insert(index, 0x00); // An explicit empty push.
        }
        4 => {
            const MARKERS: [&[u8]; 4] = [
                b"via_inscription_protocol",
                b"VIA_PROTOCOL",
                b"VIA_WI",
                b"L1ToL2Message",
            ];
            let marker_offset = MARKERS.iter().find_map(|marker| {
                bytes
                    .windows(marker.len())
                    .position(|window| window == *marker)
            });
            if let Some(offset) = marker_offset {
                bytes[offset] ^= 0x20;
            } else if let Some(first) = bytes.first_mut() {
                *first ^= 0x80;
            }
        }
        _ => unreachable!(),
    }
}

fn mutated_case(template: &CarrierTemplate, index: usize, rng: &mut StdRng) -> CorpusCase {
    let operation = index % 5;
    let operation_name = [
        "truncate",
        "corrupt",
        "oversize",
        "empty-push",
        "wrong-marker",
    ][operation];
    let mut tx = template.tx.clone();
    match template.target {
        CarrierTarget::Witness { input, item } => {
            let mut witness_items: Vec<Vec<u8>> =
                tx.input[input].witness.iter().map(<[u8]>::to_vec).collect();
            mutate_bytes(&mut witness_items[item], operation, rng);
            tx.input[input].witness = Witness::from_slice(&witness_items);
        }
        CarrierTarget::Output { index } => {
            if operation == 2 {
                tx.output[index].script_pubkey = huge_op_return(rng.gen_range(521..=2_048), 0xa5);
            } else if operation == 3 {
                tx.output[index].script_pubkey = ScriptBuf::from_bytes(vec![OP_RETURN.to_u8(), 0]);
            } else {
                let mut script = tx.output[index].script_pubkey.clone().into_bytes();
                mutate_bytes(&mut script, operation, rng);
                tx.output[index].script_pubkey = ScriptBuf::from_bytes(script);
            }
        }
    }
    CorpusCase {
        tx,
        context: template.context,
        description: format!("{}:{operation_name}", template.name),
    }
}

fn random_script(rng: &mut StdRng, wallets: &SystemWallets) -> ScriptBuf {
    match rng.gen_range(0..8) {
        0 => ScriptBuf::new(),
        1 => wallets.bridge.script_pubkey(),
        2 => wallets.sequencer.script_pubkey(),
        3 => wallets.governance.script_pubkey(),
        4 => wallets.verifiers[0].script_pubkey(),
        5 => p2wpkh(rng.gen_range(1..=200), Network::Regtest)
            .0
            .script_pubkey(),
        6 => {
            let mut payload = vec![0; rng.gen_range(0..=64)];
            rng.fill_bytes(&mut payload);
            op_return(payload).script_pubkey
        }
        _ => {
            let mut raw = vec![0; rng.gen_range(0..=96)];
            rng.fill_bytes(&mut raw);
            ScriptBuf::from_bytes(raw)
        }
    }
}

fn random_witness(rng: &mut StdRng) -> Witness {
    let item_count = rng.gen_range(0..=5);
    let mut items = Vec::with_capacity(item_count);
    for _ in 0..item_count {
        let mut item = vec![0; rng.gen_range(0..=96)];
        rng.fill_bytes(&mut item);
        items.push(item);
    }
    Witness::from_slice(&items)
}

fn random_case(index: usize, rng: &mut StdRng, wallets: &SystemWallets) -> CorpusCase {
    let input_count = rng.gen_range(0..=8);
    let output_count = rng.gen_range(0..=8);
    let mut inputs = Vec::with_capacity(input_count);
    for _ in 0..input_count {
        let mut txid = [0; 32];
        rng.fill_bytes(&mut txid);
        inputs.push(TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array(txid),
                vout: rng.gen(),
            },
            script_sig: random_script(rng, wallets),
            sequence: Sequence(rng.gen()),
            witness: random_witness(rng),
        });
    }
    let mut outputs = Vec::with_capacity(output_count);
    for _ in 0..output_count {
        outputs.push(TxOut {
            value: Amount::from_sat(rng.gen_range(0..=100_000_000)),
            script_pubkey: random_script(rng, wallets),
        });
    }
    CorpusCase {
        tx: Transaction {
            version: if rng.gen_bool(0.5) {
                Version::ONE
            } else {
                Version::TWO
            },
            lock_time: LockTime::from_consensus(rng.gen()),
            input: inputs,
            output: outputs,
        },
        context: if index % 17 == 0 {
            ContextKind::Unbootstrapped
        } else {
            ContextKind::Bootstrapped
        },
        description: format!("random-inputs-{input_count}-outputs-{output_count}"),
    }
}

fn structure_case(index: usize, rng: &mut StdRng) -> CorpusCase {
    match index % 6 {
        0 => CorpusCase {
            tx: transaction(vec![], vec![]),
            context: ContextKind::Unbootstrapped,
            description: "zero-inputs-zero-outputs".into(),
        },
        1 => CorpusCase {
            tx: transaction(
                vec![],
                vec![TxOut {
                    value: Amount::ZERO,
                    script_pubkey: huge_op_return(rng.gen_range(2_048..=8_192), rng.gen()),
                }],
            ),
            context: ContextKind::Bootstrapped,
            description: "huge-op-return".into(),
        },
        2 => {
            let inputs = (0..96)
                .map(|i| external_input(seeded_outpoint((i + 1) as u8), random_witness(rng)))
                .collect();
            CorpusCase {
                tx: transaction(inputs, vec![bridge_output(1)]),
                context: ContextKind::Bootstrapped,
                description: "many-inputs".into(),
            }
        }
        3 => {
            let duplicate = OutPoint {
                txid: Txid::from_byte_array([0xd1; 32]),
                vout: 7,
            };
            let inputs = (0..32)
                .map(|_| TxIn {
                    previous_output: duplicate,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::new(),
                })
                .collect();
            CorpusCase {
                tx: transaction(inputs, vec![]),
                context: ContextKind::Bootstrapped,
                description: "duplicate-outpoints".into(),
            }
        }
        4 => CorpusCase {
            tx: transaction(
                vec![
                    external_input(
                        seeded_outpoint(0xe1),
                        Witness::from_slice(&[Vec::<u8>::new()]),
                    ),
                    external_input(
                        seeded_outpoint(0xe2),
                        Witness::from_slice(&[vec![0], vec![1]]),
                    ),
                ],
                vec![op_return(Vec::<u8>::new())],
            ),
            context: ContextKind::Bootstrapped,
            description: "witness-items-size-zero-and-one".into(),
        },
        _ => {
            let bridge_script = wallets().bridge.script_pubkey();
            let outputs = (0..128)
                .map(|i| TxOut {
                    value: Amount::from_sat(i),
                    script_pubkey: if i % 7 == 0 {
                        bridge_script.clone()
                    } else {
                        ScriptBuf::new()
                    },
                })
                .collect();
            CorpusCase {
                tx: transaction(
                    vec![external_input(seeded_outpoint(0xf1), Witness::new())],
                    outputs,
                ),
                context: ContextKind::Bootstrapped,
                description: "many-outputs".into(),
            }
        }
    }
}

fn absent(key: &DependencyKey) -> ResolvedDependency {
    match key {
        DependencyKey::RawTx(_) => ResolvedDependency::RawTx(Resolution::KnownAbsent),
        DependencyKey::TrackedOutput(_) => {
            ResolvedDependency::TrackedOutput(Resolution::KnownAbsent)
        }
    }
}

fn run_engine(tx: &Transaction, context: &ProtocolContext, case_index: usize) -> EngineResult {
    let engine = ViaProtocolEngine::new(Network::Regtest);
    let height = case_index as u64 + 1;
    let envelope = BitcoinBlockEnvelope {
        network: Network::Regtest,
        anchor: BlockAnchor {
            height,
            hash: BlockHash::from_byte_array([(case_index as u8).wrapping_add(1); 32]),
            prev_hash: BlockHash::from_byte_array([case_index as u8; 32]),
            time: 1_700_000_000_u32.wrapping_add(case_index as u32),
        },
        transactions: vec![tx.clone()],
    };
    let (mut draft, initial_keys) = engine.inspect(&envelope, context);
    let mut dependencies: BTreeMap<DependencyKey, ResolvedDependency> = initial_keys
        .into_iter()
        .map(|key| {
            let value = absent(&key);
            (key, value)
        })
        .collect();

    let mut pending = Vec::new();
    for _round in 0..4 {
        match engine.finalize(draft, &dependencies) {
            Err(error) => return EngineResult::Failure(format!("{error:?}")),
            Ok(FinalizeOutcome::Complete(plan)) => {
                if let Err(error) = plan.validate() {
                    return EngineResult::InvalidPlan(format!("validate: {error:?}"));
                }
                let canonical = match plan.canonical_bytes() {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        return EngineResult::InvalidPlan(format!("canonical_bytes: {error:?}"))
                    }
                };
                let hash = match plan.plan_hash() {
                    Ok(hash) => hash,
                    Err(error) => {
                        return EngineResult::InvalidPlan(format!("plan_hash: {error:?}"))
                    }
                };
                return EngineResult::Complete { canonical, hash };
            }
            Ok(FinalizeOutcome::NeedDependencies { draft: next, keys }) => {
                pending = keys.clone();
                for key in &keys {
                    dependencies.insert(key.clone(), absent(key));
                }
                draft = next;
            }
        }
    }
    EngineResult::DependencyCap(pending)
}

fn panic_text(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn check_case(
    generator: &str,
    seed: u64,
    index: usize,
    case: CorpusCase,
    parser_wallets: &SystemWallets,
    bootstrapped_context: &ProtocolContext,
    unbootstrapped_context: &ProtocolContext,
) {
    let parser_context = match case.context {
        ContextKind::Bootstrapped => Some(parser_wallets),
        ContextKind::Unbootstrapped => None,
    };
    let parse_once = || {
        PositionedMessageParser::new(Network::Regtest).parse_transaction(
            &case.tx,
            0,
            index as u32,
            parser_context,
        )
    };
    let first = catch_unwind(AssertUnwindSafe(parse_once)).unwrap_or_else(|panic| {
        panic!(
            "P1 parser totality violation: generator={generator} seed={seed:#018x} index={index} case={} panic={}",
            case.description,
            panic_text(panic)
        )
    });
    let second = catch_unwind(AssertUnwindSafe(parse_once)).unwrap_or_else(|panic| {
        panic!(
            "P1 parser repeat totality violation: generator={generator} seed={seed:#018x} index={index} case={} panic={}",
            case.description,
            panic_text(panic)
        )
    });
    assert_eq!(
        first, second,
        "P2 parser determinism violation: generator={generator} seed={seed:#018x} index={index} case={}",
        case.description
    );

    let context = match case.context {
        ContextKind::Bootstrapped => bootstrapped_context,
        ContextKind::Unbootstrapped => unbootstrapped_context,
    };
    let engine_once = || run_engine(&case.tx, context, index);
    let first_engine = catch_unwind(AssertUnwindSafe(engine_once)).unwrap_or_else(|panic| {
        panic!(
            "P3 engine totality violation: generator={generator} seed={seed:#018x} index={index} case={} panic={}",
            case.description,
            panic_text(panic)
        )
    });
    let second_engine = catch_unwind(AssertUnwindSafe(engine_once)).unwrap_or_else(|panic| {
        panic!(
            "P3 engine repeat totality violation: generator={generator} seed={seed:#018x} index={index} case={} panic={}",
            case.description,
            panic_text(panic)
        )
    });
    assert_eq!(
        first_engine, second_engine,
        "P3 engine determinism violation: generator={generator} seed={seed:#018x} index={index} case={}",
        case.description
    );
    assert!(
        !matches!(first_engine, EngineResult::InvalidPlan(_)),
        "P3 completed-plan validity violation: generator={generator} seed={seed:#018x} index={index} case={} result={first_engine:?}",
        case.description
    );
}

#[test]
fn seeded_ingestion_totality_and_determinism_corpus() {
    eprintln!(
        "property corpus counts: random={RANDOM_COUNT} mutated={MUTATION_COUNT} structure={STRUCTURE_COUNT} total={}",
        RANDOM_COUNT + MUTATION_COUNT + STRUCTURE_COUNT
    );

    let parser_wallets = wallets();
    let bootstrapped_context = protocol_context(ContextKind::Bootstrapped);
    let unbootstrapped_context = protocol_context(ContextKind::Unbootstrapped);
    let mut random_rng = StdRng::seed_from_u64(RANDOM_SEED);
    for index in 0..RANDOM_COUNT {
        check_case(
            "random-valid-ish",
            RANDOM_SEED,
            index,
            random_case(index, &mut random_rng, &parser_wallets),
            &parser_wallets,
            &bootstrapped_context,
            &unbootstrapped_context,
        );
    }

    let templates = carrier_templates();
    let mut mutation_rng = StdRng::seed_from_u64(MUTATION_SEED);
    for index in 0..MUTATION_COUNT {
        let template = &templates[index % templates.len()];
        check_case(
            "mutated-protocol-carrier",
            MUTATION_SEED,
            index,
            mutated_case(template, index, &mut mutation_rng),
            &parser_wallets,
            &bootstrapped_context,
            &unbootstrapped_context,
        );
    }

    let mut structure_rng = StdRng::seed_from_u64(STRUCTURE_SEED);
    for index in 0..STRUCTURE_COUNT {
        check_case(
            "structure-edge-case",
            STRUCTURE_SEED,
            index,
            structure_case(index, &mut structure_rng),
            &parser_wallets,
            &bootstrapped_context,
            &unbootstrapped_context,
        );
    }
}
