use bitcoin::{
    absolute::LockTime,
    address::NetworkUnchecked,
    hashes::Hash,
    key::UntweakedPublicKey,
    opcodes::{
        all::{OP_CHECKSIG, OP_ENDIF, OP_IF, OP_RETURN},
        OP_FALSE, OP_TRUE,
    },
    script::{Builder, PushBytesBuf},
    secp256k1::{Keypair, Secp256k1, SecretKey},
    taproot::{LeafVersion, TaprootBuilder},
    transaction::Version,
    Address, Amount, CompressedPublicKey, Network, OutPoint, ScriptBuf, Sequence, Transaction,
    TxIn, TxOut, Txid, Witness,
};
use via_btc_ingestion::{Hash32, ProtocolVersionTag, WalletSet};
use zksync_types::{
    protocol_version::{ProtocolSemanticVersion, ProtocolVersionId, VersionPatch},
    Address as EvmAddress, L1BatchNumber, H256,
};

use crate::types::{
    self, InscriptionMessage, L1BatchDAReferenceInput, ProofDAReferenceInput,
    SystemBootstrappingInput, SystemContractUpgradeProposalInput, ValidatorAttestationInput, Vote,
};

pub const SEQUENCER_SEED: u8 = 11;
pub const GOVERNANCE_SEED: u8 = 12;
pub const VERIFIER_SEED: u8 = 13;
pub const BRIDGE_SEED: u8 = 14;

const UNAUTHORIZED_SEED: u8 = 99;
const BOOTSTRAP_INSCRIPTION_SEED: u8 = 51;
const BATCH_INSCRIPTION_SEED: u8 = 52;
const PROOF_INSCRIPTION_SEED: u8 = 53;
const ATTESTATION_INSCRIPTION_SEED: u8 = 54;
const UPGRADE_INSCRIPTION_SEED: u8 = 55;

pub fn keypair(seed: u8) -> Keypair {
    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(&[seed; 32]).unwrap();
    Keypair::from_secret_key(&secp, &secret)
}

pub fn p2wpkh(seed: u8, network: Network) -> (Address, Witness) {
    let public_key = keypair(seed).public_key();
    let address = Address::p2wpkh(&CompressedPublicKey(public_key), network);
    let witness = Witness::from_slice(&[vec![0; 71], public_key.serialize().to_vec()]);
    (address, witness)
}

pub fn p2tr(seed: u8, network: Network) -> Address {
    let secp = Secp256k1::new();
    Address::p2tr(&secp, keypair(seed).x_only_public_key().0, None, network)
}

pub fn seeded_outpoint(tag: u8) -> OutPoint {
    OutPoint {
        txid: Txid::from_byte_array([tag; 32]),
        vout: u32::from(tag),
    }
}

pub fn external_input(previous_output: OutPoint, witness: Witness) -> TxIn {
    TxIn {
        previous_output,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness,
    }
}

pub fn transaction(input: Vec<TxIn>, output: Vec<TxOut>) -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input,
        output,
    }
}

pub fn op_return(payload: Vec<u8>) -> TxOut {
    TxOut {
        value: Amount::ZERO,
        script_pubkey: Builder::new()
            .push_opcode(OP_RETURN)
            .push_slice(PushBytesBuf::try_from(payload).unwrap())
            .into_script(),
    }
}

pub fn push(data: impl AsRef<[u8]>) -> PushBytesBuf {
    PushBytesBuf::try_from(data.as_ref().to_vec()).unwrap()
}

pub fn checked_address_string(address: &Address<NetworkUnchecked>, network: Network) -> String {
    address
        .clone()
        .require_network(network)
        .unwrap()
        .to_string()
}

pub fn inscription_script(
    message: &InscriptionMessage,
    internal_key: UntweakedPublicKey,
    network: Network,
) -> ScriptBuf {
    use zksync_types::ethabi::ethereum_types::BigEndianHash;

    let mut builder = Builder::new()
        .push_slice(internal_key.serialize())
        .push_opcode(OP_CHECKSIG)
        .push_opcode(OP_FALSE)
        .push_opcode(OP_IF)
        .push_slice(push(types::VIA_INSCRIPTION_PROTOCOL));
    builder = match message {
        InscriptionMessage::L1BatchDAReference(input) => builder
            .push_slice(&*types::L1_BATCH_DA_REFERENCE_MSG)
            .push_slice(push(input.l1_batch_hash.as_bytes()))
            .push_slice(push(input.l1_batch_index.to_be_bytes()))
            .push_slice(push(input.da_identifier.as_bytes()))
            .push_slice(push(input.blob_id.as_bytes()))
            .push_slice(push(input.prev_l1_batch_hash.as_bytes())),
        InscriptionMessage::ProofDAReference(input) => builder
            .push_slice(&*types::PROOF_DA_REFERENCE_MSG)
            .push_slice(push(input.l1_batch_reveal_txid.as_byte_array()))
            .push_slice(push(input.da_identifier.as_bytes()))
            .push_slice(push(input.blob_id.as_bytes())),
        InscriptionMessage::ValidatorAttestation(input) => {
            let builder = builder
                .push_slice(&*types::VALIDATOR_ATTESTATION_MSG)
                .push_slice(push(input.reference_txid.as_byte_array()));
            match input.attestation {
                Vote::Ok => builder.push_opcode(OP_TRUE),
                Vote::NotOk => builder.push_opcode(OP_FALSE),
            }
        }
        InscriptionMessage::SystemBootstrapping(input) => {
            let mut builder = builder
                .push_slice(&*types::SYSTEM_BOOTSTRAPPING_MSG)
                .push_slice(push(input.start_block_height.to_be_bytes()))
                .push_slice(push(
                    H256::from_uint(&input.protocol_version.pack()).as_bytes(),
                ))
                .push_slice(push(input.bootloader_hash.as_bytes()))
                .push_slice(push(input.abstract_account_hash.as_bytes()))
                .push_slice(push(input.snark_wrapper_vk_hash.as_bytes()))
                .push_slice(push(input.evm_emulator_hash.as_bytes()))
                .push_slice(push(checked_address_string(
                    &input.governance_address,
                    network,
                )))
                .push_slice(push(checked_address_string(
                    &input.sequencer_address,
                    network,
                )))
                .push_slice(push(checked_address_string(
                    &input.bridge_musig2_address,
                    network,
                )));
            for verifier in &input.verifier_p2wpkh_addresses {
                builder = builder.push_slice(push(checked_address_string(verifier, network)));
            }
            builder
        }
        InscriptionMessage::L1ToL2Message(input) => builder
            .push_slice(&*types::L1_TO_L2_MSG)
            .push_slice(push(input.receiver_l2_address.as_bytes()))
            .push_slice(push(input.l2_contract_address.as_bytes()))
            .push_slice(push(&input.call_data)),
        InscriptionMessage::SystemContractUpgradeProposal(input) => {
            let mut builder = builder
                .push_slice(&*types::SYSTEM_CONTRACT_UPGRADE_MSG)
                .push_slice(push(H256::from_uint(&input.version.pack()).as_bytes()))
                .push_slice(push(input.bootloader_code_hash.as_bytes()))
                .push_slice(push(input.default_account_code_hash.as_bytes()))
                .push_slice(push(input.recursion_scheduler_level_vk_hash.as_bytes()));
            for (address, hash) in &input.system_contracts {
                builder = builder
                    .push_slice(push(address.as_bytes()))
                    .push_slice(push(hash.as_bytes()));
            }
            builder
        }
        InscriptionMessage::UpdateBridgeProposal(_) => {
            panic!("update bridge proposals are not used by this test encoder")
        }
    };
    builder.push_opcode(OP_ENDIF).into_script()
}

pub fn inscription_witness(message: &InscriptionMessage, seed: u8, network: Network) -> Witness {
    let internal_key: UntweakedPublicKey = keypair(seed).x_only_public_key().0;
    let script = inscription_script(message, internal_key, network);
    inscription_witness_for_script(script, internal_key)
}

pub fn inscription_witness_for_script(
    script: ScriptBuf,
    internal_key: UntweakedPublicKey,
) -> Witness {
    let secp = Secp256k1::new();
    let spend_info = TaprootBuilder::new()
        .add_leaf(0, script.clone())
        .unwrap()
        .finalize(&secp, internal_key)
        .unwrap();
    let control = spend_info
        .control_block(&(script.clone(), LeafVersion::TapScript))
        .unwrap();
    Witness::from_slice(&[vec![0; 64], script.into_bytes(), control.serialize()])
}

pub fn inscribed_tx(
    prev: OutPoint,
    signer_seed: u8,
    inscription_seed: u8,
    message: InscriptionMessage,
    outputs: Vec<TxOut>,
    network: Network,
) -> Transaction {
    let signer = p2wpkh(signer_seed, network).1;
    transaction(
        vec![
            external_input(prev, signer),
            external_input(
                seeded_outpoint(inscription_seed.wrapping_add(1)),
                inscription_witness(&message, inscription_seed, network),
            ),
        ],
        outputs,
    )
}

pub fn rotation_tx(input: OutPoint, role_prefix: &[u8], role_value: &[u8]) -> Transaction {
    let mut payload = role_prefix.to_vec();
    payload.push(b':');
    payload.extend_from_slice(role_value);
    transaction(
        vec![external_input(input, Witness::new())],
        vec![op_return(payload)],
    )
}

pub fn activation_tx(governance_outpoint: OutPoint, proposal_txid: Txid) -> Transaction {
    let mut payload = b"VIA_PROTOCOL:UPGRADE:".to_vec();
    payload.extend_from_slice(proposal_txid.as_byte_array());
    transaction(
        vec![external_input(governance_outpoint, Witness::new())],
        vec![op_return(payload)],
    )
}

#[derive(Clone, Copy, Debug)]
pub struct TestMessageEncoder {
    network: Network,
}

impl TestMessageEncoder {
    pub fn new(network: Network) -> Self {
        Self { network }
    }

    pub fn wallet_set(&self) -> WalletSet {
        WalletSet {
            sequencer: p2wpkh(SEQUENCER_SEED, self.network).0.script_pubkey(),
            bridge: p2tr(BRIDGE_SEED, self.network).script_pubkey(),
            governance: p2wpkh(GOVERNANCE_SEED, self.network).0.script_pubkey(),
            verifiers: vec![p2wpkh(VERIFIER_SEED, self.network).0.script_pubkey()],
        }
    }

    pub fn bootstrap_tx(
        &self,
        wallets: &WalletSet,
        version: ProtocolVersionTag,
        prev: OutPoint,
    ) -> Transaction {
        let address = |script: &ScriptBuf| {
            Address::from_script(script, self.network)
                .unwrap()
                .to_string()
                .parse::<Address<NetworkUnchecked>>()
                .unwrap()
        };
        let minor = ProtocolVersionId::try_from(u16::try_from(version.minor).unwrap()).unwrap();
        let message = InscriptionMessage::SystemBootstrapping(SystemBootstrappingInput {
            start_block_height: 0,
            protocol_version: ProtocolSemanticVersion::new(minor, VersionPatch(version.patch)),
            bootloader_hash: H256::from([1; 32]),
            abstract_account_hash: H256::from([2; 32]),
            snark_wrapper_vk_hash: H256::from([3; 32]),
            evm_emulator_hash: H256::from([4; 32]),
            governance_address: address(&wallets.governance),
            sequencer_address: address(&wallets.sequencer),
            bridge_musig2_address: address(&wallets.bridge),
            verifier_p2wpkh_addresses: wallets.verifiers.iter().map(address).collect(),
        });
        inscribed_tx(
            prev,
            SEQUENCER_SEED,
            BOOTSTRAP_INSCRIPTION_SEED,
            message,
            vec![],
            self.network,
        )
    }

    pub fn batch_da_reference_tx(
        &self,
        l1_batch_index: u64,
        l1_batch_hash: Hash32,
        blob_id: &str,
        prev: OutPoint,
    ) -> Transaction {
        let message = InscriptionMessage::L1BatchDAReference(L1BatchDAReferenceInput {
            l1_batch_hash: H256::from(l1_batch_hash),
            l1_batch_index: L1BatchNumber(u32::try_from(l1_batch_index).unwrap()),
            da_identifier: "test-da".into(),
            blob_id: blob_id.into(),
            prev_l1_batch_hash: H256::zero(),
        });
        inscribed_tx(
            prev,
            SEQUENCER_SEED,
            BATCH_INSCRIPTION_SEED,
            message,
            vec![],
            self.network,
        )
    }

    pub fn proof_da_reference_tx(
        &self,
        l1_batch_reveal_txid: Txid,
        blob_id: &str,
        prev: OutPoint,
    ) -> Transaction {
        let message = InscriptionMessage::ProofDAReference(ProofDAReferenceInput {
            l1_batch_reveal_txid,
            da_identifier: "test-da".into(),
            blob_id: blob_id.into(),
        });
        inscribed_tx(
            prev,
            SEQUENCER_SEED,
            PROOF_INSCRIPTION_SEED,
            message,
            vec![],
            self.network,
        )
    }

    pub fn attestation_tx(
        &self,
        reference_txid: Txid,
        ok: bool,
        attester_index: usize,
        prev: OutPoint,
    ) -> Transaction {
        assert_eq!(attester_index, 0, "the seeded wallet set has one verifier");
        self.signed_attestation_tx(reference_txid, ok, VERIFIER_SEED, prev)
    }

    pub fn attestations_tx(
        &self,
        reference_txid: Txid,
        attestations: &[(bool, usize, OutPoint)],
    ) -> Transaction {
        assert!(
            !attestations.is_empty(),
            "at least one attestation is required"
        );
        let mut inputs = Vec::with_capacity(attestations.len() * 2);
        for (position, (ok, attester_index, prev)) in attestations.iter().enumerate() {
            assert_eq!(*attester_index, 0, "the seeded wallet set has one verifier");
            let message = InscriptionMessage::ValidatorAttestation(ValidatorAttestationInput {
                reference_txid,
                attestation: if *ok { Vote::Ok } else { Vote::NotOk },
            });
            let offset = u8::try_from(position).expect("test attestation count fits in u8") * 2;
            let inscription_seed = ATTESTATION_INSCRIPTION_SEED.wrapping_add(offset);
            inputs.push(external_input(*prev, p2wpkh(VERIFIER_SEED, self.network).1));
            inputs.push(external_input(
                seeded_outpoint(inscription_seed.wrapping_add(1)),
                inscription_witness(&message, inscription_seed, self.network),
            ));
        }
        transaction(inputs, vec![])
    }

    pub fn unauthorized_attestation_tx(&self, reference_txid: Txid, prev: OutPoint) -> Transaction {
        self.signed_attestation_tx(reference_txid, true, UNAUTHORIZED_SEED, prev)
    }

    fn signed_attestation_tx(
        &self,
        reference_txid: Txid,
        ok: bool,
        signer_seed: u8,
        prev: OutPoint,
    ) -> Transaction {
        let message = InscriptionMessage::ValidatorAttestation(ValidatorAttestationInput {
            reference_txid,
            attestation: if ok { Vote::Ok } else { Vote::NotOk },
        });
        inscribed_tx(
            prev,
            signer_seed,
            ATTESTATION_INSCRIPTION_SEED,
            message,
            vec![],
            self.network,
        )
    }

    pub fn upgrade_proposal_tx(
        &self,
        version: ProtocolVersionTag,
        system_contracts: Vec<([u8; 20], [u8; 32])>,
        prev: OutPoint,
    ) -> Transaction {
        assert!(
            !system_contracts.is_empty(),
            "at least one system contract is required"
        );
        let minor = ProtocolVersionId::try_from(u16::try_from(version.minor).unwrap()).unwrap();
        let message =
            InscriptionMessage::SystemContractUpgradeProposal(SystemContractUpgradeProposalInput {
                version: ProtocolSemanticVersion::new(minor, VersionPatch(version.patch)),
                bootloader_code_hash: H256::from([5; 32]),
                default_account_code_hash: H256::from([6; 32]),
                evm_emulator_code_hash: None,
                recursion_scheduler_level_vk_hash: H256::from([7; 32]),
                system_contracts: system_contracts
                    .into_iter()
                    .map(|(address, hash)| (EvmAddress::from(address), H256::from(hash)))
                    .collect(),
            });
        inscribed_tx(
            prev,
            SEQUENCER_SEED,
            UPGRADE_INSCRIPTION_SEED,
            message,
            vec![],
            self.network,
        )
    }

    pub fn upgrade_activation_tx(&self, proposal_txid: Txid, gov_prev: OutPoint) -> Transaction {
        activation_tx(gov_prev, proposal_txid)
    }

    pub fn sequencer_rotation_tx(&self, new_script: &ScriptBuf, gov_prev: OutPoint) -> Transaction {
        let new_address = Address::from_script(new_script, self.network).unwrap();
        rotation_tx(
            gov_prev,
            b"VIA_PROTOCOL:SEQ",
            new_address.to_string().as_bytes(),
        )
    }

    pub fn withdrawal_tx(
        &self,
        withdrawals: &[(ScriptBuf, u64, [u8; 8])],
        bridge_prev: OutPoint,
    ) -> Transaction {
        let mut payload = b"VIA_WI".to_vec();
        payload.push(0);
        let mut outputs = Vec::with_capacity(withdrawals.len() + 1);
        for (output_index, (receiver, amount_sat, l2_id)) in withdrawals.iter().enumerate() {
            payload.extend_from_slice(l2_id);
            payload.extend_from_slice(&u16::try_from(output_index).unwrap().to_be_bytes());
            outputs.push(TxOut {
                value: Amount::from_sat(*amount_sat),
                script_pubkey: receiver.clone(),
            });
        }
        outputs.push(op_return(payload));
        transaction(vec![external_input(bridge_prev, Witness::new())], outputs)
    }
}

#[cfg(feature = "testonly")]
impl via_btc_ingestion_tests::MessageTxBuilder for TestMessageEncoder {
    fn wallet_set(&self) -> WalletSet {
        TestMessageEncoder::wallet_set(self)
    }

    fn bootstrap_tx(
        &self,
        wallets: &WalletSet,
        version: ProtocolVersionTag,
        prev: OutPoint,
    ) -> Transaction {
        TestMessageEncoder::bootstrap_tx(self, wallets, version, prev)
    }

    fn batch_da_reference_tx(
        &self,
        l1_batch_index: u64,
        l1_batch_hash: Hash32,
        blob_id: &str,
        prev: OutPoint,
    ) -> Transaction {
        TestMessageEncoder::batch_da_reference_tx(
            self,
            l1_batch_index,
            l1_batch_hash,
            blob_id,
            prev,
        )
    }

    fn proof_da_reference_tx(
        &self,
        l1_batch_reveal_txid: Txid,
        blob_id: &str,
        prev: OutPoint,
    ) -> Transaction {
        TestMessageEncoder::proof_da_reference_tx(self, l1_batch_reveal_txid, blob_id, prev)
    }

    fn attestation_tx(
        &self,
        reference_txid: Txid,
        ok: bool,
        attester_index: usize,
        prev: OutPoint,
    ) -> Transaction {
        TestMessageEncoder::attestation_tx(self, reference_txid, ok, attester_index, prev)
    }

    fn attestations_tx(
        &self,
        reference_txid: Txid,
        attestations: &[(bool, usize, OutPoint)],
    ) -> Transaction {
        TestMessageEncoder::attestations_tx(self, reference_txid, attestations)
    }

    fn unauthorized_attestation_tx(&self, reference_txid: Txid, prev: OutPoint) -> Transaction {
        TestMessageEncoder::unauthorized_attestation_tx(self, reference_txid, prev)
    }

    fn upgrade_proposal_tx(
        &self,
        version: ProtocolVersionTag,
        system_contracts: Vec<([u8; 20], [u8; 32])>,
        prev: OutPoint,
    ) -> Transaction {
        TestMessageEncoder::upgrade_proposal_tx(self, version, system_contracts, prev)
    }

    fn upgrade_activation_tx(&self, proposal_txid: Txid, gov_prev: OutPoint) -> Transaction {
        TestMessageEncoder::upgrade_activation_tx(self, proposal_txid, gov_prev)
    }

    fn sequencer_rotation_tx(&self, new_script: &ScriptBuf, gov_prev: OutPoint) -> Transaction {
        TestMessageEncoder::sequencer_rotation_tx(self, new_script, gov_prev)
    }

    fn withdrawal_tx(
        &self,
        withdrawals: &[(ScriptBuf, u64, [u8; 8])],
        bridge_prev: OutPoint,
    ) -> Transaction {
        TestMessageEncoder::withdrawal_tx(self, withdrawals, bridge_prev)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bitcoin::{hashes::Hash, BlockHash, Txid};
    use via_btc_ingestion::{
        BitcoinBlockEnvelope, BlockAnchor, DependencyKey, FinalizeOutcome, ProtocolContext,
        ProtocolEngine, ProtocolEvent, Resolution, ResolvedDependency,
    };
    use zksync_types::via_wallet::SystemWallets;

    use super::*;
    use crate::{
        indexer::positioned::{PositionedMessageParser, PositionedParseOutcome},
        types::FullInscriptionMessage,
    };

    fn parser_wallets(encoder: &TestMessageEncoder) -> SystemWallets {
        let wallets = encoder.wallet_set();
        let address = |script: &ScriptBuf| Address::from_script(script, encoder.network).unwrap();
        SystemWallets {
            sequencer: address(&wallets.sequencer),
            bridge: address(&wallets.bridge),
            governance: address(&wallets.governance),
            verifiers: wallets.verifiers.iter().map(address).collect(),
        }
    }

    fn messages(tx: &Transaction, encoder: &TestMessageEncoder) -> Vec<FullInscriptionMessage> {
        let wallets = parser_wallets(encoder);
        PositionedMessageParser::new(encoder.network)
            .parse_transaction(tx, 0, 100, Some(&wallets))
            .into_iter()
            .filter_map(|outcome| match outcome {
                PositionedParseOutcome::Valid(message) => Some(message.message),
                _ => None,
            })
            .collect()
    }

    fn prev(tag: u8) -> OutPoint {
        seeded_outpoint(tag)
    }

    #[test]
    fn message_builder_transactions_round_trip() {
        let encoder = TestMessageEncoder::new(Network::Regtest);
        let wallets = encoder.wallet_set();
        let version = ProtocolVersionTag {
            minor: 26,
            patch: 0,
        };

        let bootstrap = encoder.bootstrap_tx(&wallets, version, prev(1));
        assert_eq!(bootstrap.input[0].previous_output, prev(1));
        assert!(matches!(
            &messages(&bootstrap, &encoder)[..],
            [FullInscriptionMessage::SystemBootstrapping(_)]
        ));

        let batch = encoder.batch_da_reference_tx(7, [3; 32], "batch-blob", prev(2));
        assert_eq!(batch.input[0].previous_output, prev(2));
        assert!(matches!(
            &messages(&batch, &encoder)[..],
            [FullInscriptionMessage::L1BatchDAReference(_)]
        ));

        let batch_txid = batch.compute_txid();
        let proof = encoder.proof_da_reference_tx(batch_txid, "proof-blob", prev(3));
        assert_eq!(proof.input[0].previous_output, prev(3));
        assert!(matches!(
            &messages(&proof, &encoder)[..],
            [FullInscriptionMessage::ProofDAReference(_)]
        ));

        let proof_txid = proof.compute_txid();
        let attestation = encoder.attestation_tx(proof_txid, true, 0, prev(4));
        assert_eq!(attestation.input[0].previous_output, prev(4));
        assert!(matches!(
            &messages(&attestation, &encoder)[..],
            [FullInscriptionMessage::ValidatorAttestation(message)]
                if matches!(message.input.attestation, Vote::Ok)
        ));

        let unauthorized = encoder.unauthorized_attestation_tx(proof_txid, prev(5));
        assert_eq!(unauthorized.input[0].previous_output, prev(5));
        assert!(matches!(
            &messages(&unauthorized, &encoder)[..],
            [FullInscriptionMessage::ValidatorAttestation(_)]
        ));

        let proposal = encoder.upgrade_proposal_tx(
            ProtocolVersionTag {
                minor: 27,
                patch: 0,
            },
            vec![([0x31; 20], [0x41; 32])],
            prev(6),
        );
        assert_eq!(proposal.input[0].previous_output, prev(6));
        assert!(matches!(
            &messages(&proposal, &encoder)[..],
            [FullInscriptionMessage::SystemContractUpgradeProposal(_)]
        ));

        let proposal_txid = proposal.compute_txid();
        let activation = encoder.upgrade_activation_tx(proposal_txid, prev(7));
        assert_eq!(activation.input[0].previous_output, prev(7));
        assert!(matches!(
            &messages(&activation, &encoder)[..],
            [FullInscriptionMessage::SystemContractUpgrade(_)]
        ));

        let new_sequencer = p2wpkh(77, Network::Regtest).0.script_pubkey();
        let rotation = encoder.sequencer_rotation_tx(&new_sequencer, prev(8));
        assert_eq!(rotation.input[0].previous_output, prev(8));
        assert!(matches!(
            &messages(&rotation, &encoder)[..],
            [FullInscriptionMessage::UpdateSequencer(message)]
                if message.input.address.clone().require_network(Network::Regtest).unwrap().script_pubkey()
                    == new_sequencer
        ));
    }

    #[test]
    fn upgrade_proposal_preserves_system_contract_order_through_the_engine() {
        let encoder = TestMessageEncoder::new(Network::Regtest);
        let version = ProtocolVersionTag {
            minor: 27,
            patch: 0,
        };
        let system_contracts = vec![
            ([0x11; 20], [0xa1; 32]),
            ([0x22; 20], [0xb2; 32]),
            ([0x33; 20], [0xc3; 32]),
        ];
        let tx = encoder.upgrade_proposal_tx(version, system_contracts.clone(), prev(11));

        let parsed = messages(&tx, &encoder);
        let [FullInscriptionMessage::SystemContractUpgradeProposal(message)] = &parsed[..] else {
            panic!("expected one upgrade proposal, got {parsed:?}")
        };
        let parsed_contracts: Vec<_> = message
            .input
            .system_contracts
            .iter()
            .map(|(address, hash)| (address.0, hash.0))
            .collect();
        assert_eq!(
            parsed_contracts, system_contracts,
            "parser must preserve inscription order"
        );

        let context = ProtocolContext {
            version: 1,
            wallets: encoder.wallet_set(),
            protocol_version: ProtocolVersionTag {
                minor: 26,
                patch: 0,
            },
        };
        let envelope = BitcoinBlockEnvelope {
            network: Network::Regtest,
            anchor: BlockAnchor {
                height: 100,
                hash: BlockHash::from_byte_array([0x64; 32]),
                prev_hash: BlockHash::from_byte_array([0x63; 32]),
                time: 1_700_000_000,
            },
            transactions: vec![tx],
        };
        let engine = crate::ingestion_engine::ViaProtocolEngine::new(Network::Regtest);
        let (draft, keys) = engine.inspect(&envelope, &context);
        let dependencies: BTreeMap<_, _> = keys
            .into_iter()
            .map(|key| {
                let value = match &key {
                    DependencyKey::RawTx(_) => ResolvedDependency::RawTx(Resolution::KnownAbsent),
                    DependencyKey::TrackedOutput(_) => {
                        ResolvedDependency::TrackedOutput(Resolution::KnownAbsent)
                    }
                };
                (key, value)
            })
            .collect();
        let FinalizeOutcome::Complete(plan) = engine.finalize(draft, &dependencies).unwrap() else {
            panic!("proposal block unexpectedly requested more dependencies")
        };
        let event = plan.events.iter().find_map(|event| match event {
            ProtocolEvent::SystemContractUpgradeProposal(event) => Some(event),
            _ => None,
        });
        assert_eq!(
            event
                .expect("upgrade proposal event")
                .proposal
                .system_contracts,
            system_contracts,
            "engine event must preserve system-contract identity order"
        );

        let minor = ProtocolVersionId::try_from(u16::try_from(version.minor).unwrap()).unwrap();
        let malformed_message =
            InscriptionMessage::SystemContractUpgradeProposal(SystemContractUpgradeProposalInput {
                version: ProtocolSemanticVersion::new(minor, VersionPatch(version.patch)),
                bootloader_code_hash: H256::from([5; 32]),
                default_account_code_hash: H256::from([6; 32]),
                evm_emulator_code_hash: None,
                recursion_scheduler_level_vk_hash: H256::from([7; 32]),
                system_contracts: system_contracts
                    .into_iter()
                    .map(|(address, hash)| (EvmAddress::from(address), H256::from(hash)))
                    .collect(),
            });
        let internal_key = keypair(UPGRADE_INSCRIPTION_SEED).x_only_public_key().0;
        let valid_script =
            inscription_script(&malformed_message, internal_key, Network::Regtest).into_bytes();
        let outcomes_for_script = |script, outpoint_tag| {
            let malformed_tx = transaction(
                vec![
                    external_input(
                        prev(outpoint_tag),
                        p2wpkh(SEQUENCER_SEED, Network::Regtest).1,
                    ),
                    external_input(
                        seeded_outpoint(UPGRADE_INSCRIPTION_SEED.wrapping_add(1)),
                        inscription_witness_for_script(ScriptBuf::from_bytes(script), internal_key),
                    ),
                ],
                vec![],
            );
            PositionedMessageParser::new(Network::Regtest).parse_transaction(
                &malformed_tx,
                0,
                100,
                Some(&parser_wallets(&encoder)),
            )
        };

        let mut malformed_script = valid_script.clone();
        assert_eq!(malformed_script.pop(), Some(OP_ENDIF.to_u8()));
        malformed_script.extend_from_slice(&[1, 0xdd, OP_ENDIF.to_u8()]);
        let outcomes = outcomes_for_script(malformed_script, 12);
        assert!(
            outcomes.iter().any(|outcome| matches!(
                outcome,
                PositionedParseOutcome::Malformed(malformed)
                    if malformed.location == via_btc_ingestion::MessageLocation::Input(1)
            )),
            "odd system-contract instruction counts are malformed"
        );

        let mut malformed_script = valid_script;
        assert_eq!(malformed_script.pop(), Some(OP_ENDIF.to_u8()));
        malformed_script.push(OP_CHECKSIG.to_u8());
        let outcomes = outcomes_for_script(malformed_script, 13);
        assert!(
            outcomes.iter().any(|outcome| matches!(
                outcome,
                PositionedParseOutcome::Malformed(malformed)
                    if malformed.location == via_btc_ingestion::MessageLocation::Input(1)
            )),
            "upgrade proposal must end with the inscription sentinel"
        );
    }

    #[test]
    fn withdrawal_round_trips_outputs_and_metadata() {
        let encoder = TestMessageEncoder::new(Network::Regtest);
        let receiver_a = p2wpkh(81, Network::Regtest).0;
        let receiver_b = p2wpkh(82, Network::Regtest).0;
        let withdrawals = [
            (receiver_a.script_pubkey(), 40_000, [0xa1; 8]),
            (receiver_b.script_pubkey(), 50_000, [0xb2; 8]),
        ];
        let tx = encoder.withdrawal_tx(&withdrawals, prev(9));
        assert_eq!(tx.input[0].previous_output, prev(9));
        let parsed = messages(&tx, &encoder);
        let [FullInscriptionMessage::BridgeWithdrawal(message)] = &parsed[..] else {
            panic!("expected one bridge withdrawal, got {parsed:?}")
        };
        assert_eq!(message.input.withdrawals.len(), 2);
        for (index, (actual, (receiver_script, amount, l2_id))) in message
            .input
            .withdrawals
            .iter()
            .zip(withdrawals.iter())
            .enumerate()
        {
            let mut composite_id = l2_id.to_vec();
            composite_id.extend_from_slice(&(index as u16).to_be_bytes());
            assert_eq!(actual.l2_meta.l2_id, hex::encode(composite_id));
            assert_eq!(actual.l2_meta.l2_tx_event_index, index as u16);
            assert_eq!(actual.receiver.script_pubkey(), *receiver_script);
            assert_eq!(actual.value, Amount::from_sat(*amount));
        }
    }

    #[test]
    fn generalized_rotation_helper_covers_all_role_prefixes() {
        let outpoint = prev(10);
        let address = p2wpkh(83, Network::Regtest).0;
        for prefix in [
            b"VIA_PROTOCOL:SEQ".as_slice(),
            b"VIA_PROTOCOL:GOV".as_slice(),
        ] {
            let tx = rotation_tx(outpoint, prefix, address.to_string().as_bytes());
            assert_eq!(tx.input[0].previous_output, outpoint);
        }
        let proposal_txid = Txid::from_byte_array([0xc3; 32]);
        let bridge = rotation_tx(outpoint, b"VIA_PROTOCOL:BRI", proposal_txid.as_byte_array());
        assert_eq!(bridge.input[0].previous_output, outpoint);
    }
}
