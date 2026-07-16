use std::str::FromStr;

use bitcoin::{
    address::NetworkUnchecked,
    hashes::Hash,
    opcodes::all::{OP_ENDIF, OP_RETURN},
    script::{Instruction, PushBytesBuf},
    taproot::{ControlBlock, Signature as TaprootSignature},
    Address, Amount, CompressedPublicKey, Network, ScriptBuf, Transaction, TxOut, Txid, Witness,
};
use tracing::{debug, instrument, warn};
use zksync_basic_types::H256;
use zksync_types::{
    protocol_version::ProtocolSemanticVersion, via_wallet::SystemWallets, Address as EVMAddress,
    L1BatchNumber, U256,
};

use crate::{
    indexer::withdrawal::{parse_withdrawals, L1Withdrawal, WithdrawalVersion, VIA_WI},
    types::{
        self, BridgeWithdrawal, BridgeWithdrawalInput, CommonFields, FullInscriptionMessage,
        L1BatchDAReference, L1BatchDAReferenceInput, L1ToL2Message, L1ToL2MessageInput,
        ProofDAReference, ProofDAReferenceInput, SystemBootstrapping, SystemBootstrappingInput,
        SystemContractUpgrade, SystemContractUpgradeInput, SystemContractUpgradeProposal,
        SystemContractUpgradeProposalInput, TransactionWithMetadata, UpdateBridge,
        UpdateBridgeInput, UpdateBridgeProposal, UpdateBridgeProposalInput, UpdateGovernance,
        UpdateGovernanceInput, UpdateSequencer, UpdateSequencerInput, ValidatorAttestation,
        ValidatorAttestationInput, Vote,
    },
};

const OP_RETURN_UPGRADE_PROTOCOL_PREFIX: &[u8] = b"VIA_PROTOCOL:UPGRADE";
const OP_RETURN_UPDATE_SEQUENCER_PREFIX: &[u8] = b"VIA_PROTOCOL:SEQ";
const OP_RETURN_UPDATE_BRIDGE_PREFIX: &[u8] = b"VIA_PROTOCOL:BRI";
const OP_RETURN_UPDATE_GOVERNANCE_PREFIX: &[u8] = b"VIA_PROTOCOL:GOV";
const OP_RETURN_RETIRED_WITHDRAW_PREFIX: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL";

enum OpReturnCarrier<'a> {
    One(&'a [u8]),
    Two(&'a [u8], &'a [u8]),
}

/// Accepts non-minimal push encodings because confirmed blocks may contain them.
fn first_op_return_carrier(tx: &Transaction) -> Option<OpReturnCarrier<'_>> {
    let output = tx
        .output
        .iter()
        .find(|output| output.script_pubkey.is_op_return())?;
    let mut instructions = output.script_pubkey.instructions();

    match instructions.next()? {
        Ok(Instruction::Op(opcode)) if opcode == OP_RETURN => {}
        Ok(_) | Err(_) => return None,
    }
    let first = match instructions.next()? {
        Ok(Instruction::PushBytes(bytes)) => bytes.as_bytes(),
        Ok(_) | Err(_) => return None,
    };
    match instructions.next() {
        None => Some(OpReturnCarrier::One(first)),
        Some(Ok(Instruction::PushBytes(bytes))) => match instructions.next() {
            None => Some(OpReturnCarrier::Two(first, bytes.as_bytes())),
            Some(Ok(_)) | Some(Err(_)) => None,
        },
        Some(Ok(_)) | Some(Err(_)) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveOpReturnKind {
    Withdrawal,
    ProtocolUpgrade,
    UpdateSequencer,
    UpdateBridge,
    UpdateGovernance,
}

const ACTIVE_OP_RETURN_PREFIXES: &[(ActiveOpReturnKind, &[u8])] = &[
    (ActiveOpReturnKind::Withdrawal, VIA_WI),
    (
        ActiveOpReturnKind::ProtocolUpgrade,
        OP_RETURN_UPGRADE_PROTOCOL_PREFIX,
    ),
    (
        ActiveOpReturnKind::UpdateSequencer,
        OP_RETURN_UPDATE_SEQUENCER_PREFIX,
    ),
    (
        ActiveOpReturnKind::UpdateBridge,
        OP_RETURN_UPDATE_BRIDGE_PREFIX,
    ),
    (
        ActiveOpReturnKind::UpdateGovernance,
        OP_RETURN_UPDATE_GOVERNANCE_PREFIX,
    ),
];

enum ReservedFromDeposit {
    Active(ActiveOpReturnKind),
    RetiredReserved,
    UnreservedDeposit,
}

fn active_kind_from_leading_prefix(body: &[u8]) -> Option<ActiveOpReturnKind> {
    ACTIVE_OP_RETURN_PREFIXES
        .iter()
        .find_map(|&(kind, prefix)| body.starts_with(prefix).then_some(kind))
}

fn active_kind_from_exact_prefix(prefix: &[u8]) -> Option<ActiveOpReturnKind> {
    ACTIVE_OP_RETURN_PREFIXES
        .iter()
        .find_map(|&(kind, active_prefix)| (prefix == active_prefix).then_some(kind))
}

fn reserved_from_deposit(body: &[u8]) -> ReservedFromDeposit {
    if let Some(kind) = active_kind_from_leading_prefix(body) {
        ReservedFromDeposit::Active(kind)
    } else if body.starts_with(OP_RETURN_RETIRED_WITHDRAW_PREFIX) {
        ReservedFromDeposit::RetiredReserved
    } else {
        ReservedFromDeposit::UnreservedDeposit
    }
}

fn op_return_common_fields(tx: &Transaction, block_height: u32) -> Option<CommonFields> {
    Some(CommonFields {
        schnorr_signature: TaprootSignature::from_slice(&[0; 64]).ok()?,
        encoded_public_key: PushBytesBuf::new(),
        block_height,
        tx_id: tx.compute_ntxid().into(),
        p2wpkh_address: None,
        tx_index: None,
        output_vout: None,
    })
}

fn decode_fixed_push<const N: usize>(instruction: &Instruction<'_>) -> Option<[u8; N]> {
    instruction.push_bytes()?.as_bytes().try_into().ok()
}

fn h160_push(instruction: &Instruction<'_>) -> Option<EVMAddress> {
    decode_fixed_push::<20>(instruction).map(EVMAddress::from)
}

fn h256_push(instruction: &Instruction<'_>) -> Option<H256> {
    decode_fixed_push::<32>(instruction).map(H256::from)
}

fn protocol_version_push(instruction: &Instruction<'_>) -> Option<ProtocolSemanticVersion> {
    let bytes = instruction.push_bytes()?.as_bytes();
    (bytes.len() <= 32)
        .then(|| ProtocolSemanticVersion::try_from_packed(U256::from_big_endian(bytes)))?
        .ok()
}

// Using constants to define the minimum number of instructions can help to make parsing more quick
const MIN_WITNESS_LENGTH: usize = 3;
const MIN_SYSTEM_BOOTSTRAPPING_INSTRUCTIONS: usize = 11;
const MIN_VALIDATOR_ATTESTATION_INSTRUCTIONS: usize = 4;
const MIN_L1_BATCH_DA_REFERENCE_INSTRUCTIONS: usize = 7;
const MIN_PROOF_DA_REFERENCE_INSTRUCTIONS: usize = 5;
const MIN_L1_TO_L2_MESSAGE_INSTRUCTIONS: usize = 5;
const MIN_SYSTEM_CONTRACT_UPGRADE_PROPOSAL: usize = 6;
const MIN_UPDATE_BRIDGE_PROPOSAL: usize = 5;

#[derive(Debug, Clone)]
pub struct MessageParser {
    network: Network,
}

impl MessageParser {
    pub fn new(network: Network) -> Self {
        Self { network }
    }

    #[instrument(skip(self, tx), target = "bitcoin_indexer::parser")]
    pub fn parse_system_transaction(
        &mut self,
        tx: &Transaction,
        block_height: u32,
        wallets: Option<&SystemWallets>,
    ) -> Vec<FullInscriptionMessage> {
        // parsing btc address
        let mut sender_addresses: Option<Address> = None;
        for input in tx.input.iter() {
            let witness = &input.witness;
            if let Some(btc_address) = self.parse_p2wpkh(witness) {
                sender_addresses = Some(btc_address);
            }
        }

        match sender_addresses {
            Some(address) => {
                // parsing messages
                tx.input
                    .iter()
                    .filter_map(|input| {
                        self.parse_system_input(input, tx, block_height, address.clone(), wallets)
                    })
                    .collect()
            }
            None => {
                vec![]
            }
        }
    }

    #[instrument(skip(self, tx), target = "bitcoin_indexer::parser")]
    pub fn parse_protocol_upgrade_transactions(
        &mut self,
        tx: &TransactionWithMetadata,
        block_height: u32,
    ) -> Vec<FullInscriptionMessage> {
        enum ParsedMessage {
            ProtocolUpgrade(Txid),
            UpdateBridge(Txid),
            UpdateSequencer(Address<NetworkUnchecked>),
            UpdateGovernance(Address<NetworkUnchecked>),
        }

        let OpReturnCarrier::Two(prefix, body) = (match first_op_return_carrier(&tx.tx) {
            Some(carrier) => carrier,
            None => return Vec::new(),
        }) else {
            return Vec::new();
        };
        let parsed = match active_kind_from_exact_prefix(prefix) {
            Some(ActiveOpReturnKind::Withdrawal) | None => return Vec::new(),
            Some(ActiveOpReturnKind::ProtocolUpgrade) => {
                let Some(txid) = body
                    .get(..32)
                    .and_then(|bytes| Txid::from_slice(bytes).ok())
                else {
                    return Vec::new();
                };
                ParsedMessage::ProtocolUpgrade(txid)
            }
            Some(ActiveOpReturnKind::UpdateBridge) => {
                let Some(txid) = body
                    .get(..32)
                    .and_then(|bytes| Txid::from_slice(bytes).ok())
                else {
                    return Vec::new();
                };
                ParsedMessage::UpdateBridge(txid)
            }
            Some(ActiveOpReturnKind::UpdateSequencer) => {
                let Some(address) = std::str::from_utf8(body)
                    .ok()
                    .and_then(|address| Address::from_str(address).ok())
                else {
                    return Vec::new();
                };
                ParsedMessage::UpdateSequencer(address)
            }
            Some(ActiveOpReturnKind::UpdateGovernance) => {
                let Some(address) = std::str::from_utf8(body)
                    .ok()
                    .and_then(|address| Address::from_str(address).ok())
                else {
                    return Vec::new();
                };
                ParsedMessage::UpdateGovernance(address)
            }
        };

        let inputs = tx
            .tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        let Some(common) = op_return_common_fields(&tx.tx, block_height) else {
            return Vec::new();
        };
        let message = match parsed {
            ParsedMessage::ProtocolUpgrade(proposal_tx_id) => {
                FullInscriptionMessage::SystemContractUpgrade(SystemContractUpgrade {
                    common,
                    input: SystemContractUpgradeInput {
                        inputs,
                        proposal_tx_id,
                    },
                })
            }
            ParsedMessage::UpdateBridge(proposal_tx_id) => {
                FullInscriptionMessage::UpdateBridge(UpdateBridge {
                    common,
                    input: UpdateBridgeInput {
                        inputs,
                        proposal_tx_id,
                    },
                })
            }
            ParsedMessage::UpdateSequencer(address) => {
                FullInscriptionMessage::UpdateSequencer(UpdateSequencer {
                    common,
                    input: UpdateSequencerInput { inputs, address },
                })
            }
            ParsedMessage::UpdateGovernance(address) => {
                FullInscriptionMessage::UpdateGovernance(UpdateGovernance {
                    common,
                    input: UpdateGovernanceInput { inputs, address },
                })
            }
        };

        vec![message]
    }

    #[instrument(skip(self, tx), target = "bitcoin_indexer::parser")]
    pub fn parse_bridge_transaction(
        &mut self,
        tx: &mut TransactionWithMetadata,
        block_height: u32,
        wallets: &SystemWallets,
    ) -> Vec<FullInscriptionMessage> {
        let mut messages = Vec::new();

        let vout = match tx.tx.output.iter().enumerate().find_map(|(index, output)| {
            if output.script_pubkey == wallets.bridge.script_pubkey() {
                Some(index)
            } else {
                None
            }
        }) {
            Some(index) => {
                tx.set_output_vout(index);
                index
            }
            None => return messages,
        };

        let bridge_output = &tx.tx.output[vout];

        // A valid witness deposit suppresses only a competing OP_RETURN deposit; VIA_WI
        // withdrawal parsing remains independent.
        let witness_deposit = self.parse_inscription_deposit(tx, block_height, wallets);

        let op_return_message = match first_op_return_carrier(&tx.tx) {
            Some(OpReturnCarrier::One(body)) => match reserved_from_deposit(body) {
                ReservedFromDeposit::Active(ActiveOpReturnKind::Withdrawal) => {
                    self.parse_op_return_withdrawal(&tx.tx, block_height, wallets, body)
                }
                ReservedFromDeposit::Active(_) | ReservedFromDeposit::RetiredReserved => None,
                ReservedFromDeposit::UnreservedDeposit if witness_deposit.is_none() => {
                    self.parse_op_return_deposit(tx, block_height, bridge_output, body)
                }
                ReservedFromDeposit::UnreservedDeposit => None,
            },
            Some(OpReturnCarrier::Two(_, _)) | None => None,
        };
        messages.extend(witness_deposit);
        messages.extend(op_return_message);

        messages
    }

    #[instrument(skip(self, input, tx), target = "bitcoin_indexer::parser")]
    fn parse_system_input(
        &mut self,
        input: &bitcoin::TxIn,
        tx: &Transaction,
        block_height: u32,
        address: Address,
        wallets: Option<&SystemWallets>,
    ) -> Option<FullInscriptionMessage> {
        let witness = &input.witness;
        if witness.len() < MIN_WITNESS_LENGTH {
            return None;
        }

        let signature = match TaprootSignature::from_slice(&witness[0]) {
            Ok(sig) => sig,
            Err(e) => {
                warn!("Failed to parse Taproot signature: {}", e);
                return None;
            }
        };
        let script = ScriptBuf::from_bytes(witness[1].to_vec());
        let control_block = match ControlBlock::decode(&witness[2]) {
            Ok(cb) => cb,
            Err(e) => {
                warn!("Failed to decode control block: {}", e);
                return None;
            }
        };

        let instructions: Vec<_> = script.instructions().filter_map(Result::ok).collect();
        let via_index = match find_via_inscription_protocol(&instructions) {
            Some(index) => index,
            None => {
                debug!("VIA inscription protocol not found in script");
                return None;
            }
        };

        let public_key = control_block.internal_key;
        let common_fields = CommonFields {
            schnorr_signature: signature,
            encoded_public_key: PushBytesBuf::from(public_key.serialize()),
            block_height,
            tx_id: tx.compute_ntxid().into(),
            p2wpkh_address: Some(address),
            tx_index: None,
            output_vout: None,
        };

        self.parse_system_message(tx, &instructions[via_index..], &common_fields, wallets)
    }

    #[instrument(skip(self), target = "bitcoin_indexer::parser")]
    pub fn parse_p2wpkh(&self, witness: &Witness) -> Option<Address> {
        if witness.len() == 2 {
            let public_key = bitcoin::PublicKey::from_slice(&witness[1]).ok()?;
            let cm_pk = CompressedPublicKey::try_from(public_key).ok()?;

            Some(Address::p2wpkh(&cm_pk, self.network))
        } else {
            None
        }
    }

    #[instrument(
        skip(self, tx, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_system_message(
        &mut self,
        tx: &Transaction,
        instructions: &[Instruction],
        common_fields: &CommonFields,
        wallets: Option<&SystemWallets>,
    ) -> Option<FullInscriptionMessage> {
        let message_type = instructions.get(1)?;

        match message_type {
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::SYSTEM_BOOTSTRAPPING_MSG.as_bytes() =>
            {
                debug!("Parsing system bootstrapping message");
                self.parse_system_bootstrapping(instructions, common_fields)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::VALIDATOR_ATTESTATION_MSG.as_bytes() =>
            {
                debug!("Parsing validator attestation message");
                self.parse_validator_attestation(instructions, common_fields)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::L1_BATCH_DA_REFERENCE_MSG.as_bytes() =>
            {
                debug!("Parsing L1 batch DA reference message");
                self.parse_l1_batch_da_reference(instructions, common_fields)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::PROOF_DA_REFERENCE_MSG.as_bytes() =>
            {
                debug!("Parsing proof DA reference message");
                self.parse_proof_da_reference(instructions, common_fields)
            }
            Instruction::PushBytes(bytes) if bytes.as_bytes() == types::L1_TO_L2_MSG.as_bytes() => {
                debug!("Parsing L1 to L2 message");
                self.parse_l1_to_l2_message(tx, instructions, common_fields, wallets)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::SYSTEM_CONTRACT_UPGRADE_MSG.as_bytes() =>
            {
                debug!("Parsing System contract upgrade proposal");
                self.parse_system_contract_upgrade_message(instructions, common_fields)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::UPGRADE_BRIDGE_MSG.as_bytes() =>
            {
                debug!("Parsing update bridge proposal");
                self.parse_update_bridge_proposal_message(instructions, common_fields)
            }
            Instruction::PushBytes(bytes) => {
                warn!("Unknown message type for system transaction parser");
                warn!(
                    "first instruction: {:?}",
                    String::from_utf8(bytes.as_bytes().to_vec())
                );
                None
            }
            Instruction::Op(_) => {
                warn!("Invalid message type");
                warn!("Instructions: {:?}", instructions);
                None
            }
        }
    }

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_system_bootstrapping(
        &mut self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<FullInscriptionMessage> {
        if instructions.len() < MIN_SYSTEM_BOOTSTRAPPING_INSTRUCTIONS {
            warn!("Insufficient instructions for system bootstrapping");
            return None;
        }

        let start_block_height = u32::from_be_bytes(
            instructions
                .get(2)?
                .push_bytes()?
                .as_bytes()
                .try_into()
                .ok()?,
        );
        debug!("Parsed start block height: {}", start_block_height);

        let protocol_version = protocol_version_push(instructions.get(3)?)?;
        debug!("Parsed protocol version");

        let bootloader_hash = h256_push(instructions.get(4)?)?;
        debug!("Parsed bootloader hash");

        let abstract_account_hash = h256_push(instructions.get(5)?)?;
        debug!("Parsed abstract account hash");

        let snark_wrapper_vk_hash = h256_push(instructions.get(6)?)?;

        let evm_emulator_hash = h256_push(instructions.get(7)?)?;

        let network_unchecked_governance_address = instructions.get(8).and_then(|instr| {
            if let Instruction::PushBytes(bytes) = instr {
                std::str::from_utf8(bytes.as_bytes())
                    .ok()
                    .and_then(|s| s.parse::<Address<NetworkUnchecked>>().ok())
            } else {
                None
            }
        })?;

        debug!("Parsed governance address");

        let network_unchecked_sequencer_address = instructions.get(9).and_then(|instr| {
            if let Instruction::PushBytes(bytes) = instr {
                std::str::from_utf8(bytes.as_bytes())
                    .ok()
                    .and_then(|s| s.parse::<Address<NetworkUnchecked>>().ok())
            } else {
                None
            }
        })?;

        debug!("Parsed sequencer address");

        let network_unchecked_bridge_address = instructions.get(10).and_then(|instr| {
            if let Instruction::PushBytes(bytes) = instr {
                std::str::from_utf8(bytes.as_bytes())
                    .ok()
                    .and_then(|s| s.parse::<Address<NetworkUnchecked>>().ok())
            } else {
                None
            }
        })?;

        debug!("Parsed bridge address");

        // network unchecked is required to enable serde serialization and deserialization on the library structs
        let network_unchecked_verifier_addresses = instructions[11..]
            .iter()
            .filter_map(|instr| {
                if let Instruction::PushBytes(bytes) = instr {
                    std::str::from_utf8(bytes.as_bytes())
                        .ok()
                        .and_then(|s| s.parse::<Address<NetworkUnchecked>>().ok())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        debug!(
            "Parsed {} verifier addresses",
            network_unchecked_verifier_addresses.len()
        );

        Some(FullInscriptionMessage::SystemBootstrapping(
            SystemBootstrapping {
                common: common_fields.clone(),
                input: SystemBootstrappingInput {
                    start_block_height,
                    protocol_version,
                    bootloader_hash,
                    abstract_account_hash,
                    snark_wrapper_vk_hash,
                    evm_emulator_hash,
                    governance_address: network_unchecked_governance_address,
                    sequencer_address: network_unchecked_sequencer_address,
                    bridge_musig2_address: network_unchecked_bridge_address,
                    verifier_p2wpkh_addresses: network_unchecked_verifier_addresses,
                },
            },
        ))
    }

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_validator_attestation(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<FullInscriptionMessage> {
        if instructions.len() < MIN_VALIDATOR_ATTESTATION_INSTRUCTIONS {
            warn!("Insufficient instructions for validator attestation");
            return None;
        }

        let reference_txid = match Txid::from_slice(instructions.get(2)?.push_bytes()?.as_bytes()) {
            Ok(txid) => {
                debug!("Parsed reference txid");
                txid
            }
            Err(e) => {
                warn!("Failed to parse reference txid: {}", e);
                return None;
            }
        };

        let attestation = match instructions.get(3)? {
            Instruction::PushBytes(bytes) => match bytes.as_bytes() {
                b"OP_1" => Vote::Ok,
                b"" => Vote::NotOk,
                _ => {
                    warn!("Invalid attestation value");
                    return None;
                }
            },
            Instruction::Op(op) => {
                if op.to_u8() == 0x51 {
                    Vote::Ok
                } else if op.to_u8() == 0x00 {
                    Vote::NotOk
                } else {
                    warn!("Invalid attestation value");
                    return None;
                }
            }
        };

        debug!("Parsed attestation: {:?}", attestation);

        Some(FullInscriptionMessage::ValidatorAttestation(
            ValidatorAttestation {
                common: common_fields.clone(),
                input: ValidatorAttestationInput {
                    reference_txid,
                    attestation,
                },
            },
        ))
    }

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_l1_batch_da_reference(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<FullInscriptionMessage> {
        if instructions.len() < MIN_L1_BATCH_DA_REFERENCE_INSTRUCTIONS {
            warn!("Insufficient instructions for L1 batch DA reference");
            return None;
        }

        let l1_batch_hash = h256_push(instructions.get(2)?)?;
        debug!("Parsed L1 batch hash");

        let l1_batch_index = L1BatchNumber(u32::from_be_bytes(
            instructions
                .get(3)?
                .push_bytes()?
                .as_bytes()
                .try_into()
                .ok()?,
        ));
        debug!("Parsed L1 batch index: {}", l1_batch_index);

        let da_identifier = std::str::from_utf8(instructions.get(4)?.push_bytes()?.as_bytes())
            .ok()?
            .to_string();
        debug!("Parsed DA identifier: {}", da_identifier);

        let blob_id = std::str::from_utf8(instructions.get(5)?.push_bytes()?.as_bytes())
            .ok()?
            .to_string();
        debug!("Parsed blob ID: {}", blob_id);

        let prev_l1_batch_hash = h256_push(instructions.get(6)?)?;
        debug!("Parsed previous L1 batch hash");

        Some(FullInscriptionMessage::L1BatchDAReference(
            L1BatchDAReference {
                common: common_fields.clone(),
                input: L1BatchDAReferenceInput {
                    l1_batch_hash,
                    l1_batch_index,
                    da_identifier,
                    blob_id,
                    prev_l1_batch_hash,
                },
            },
        ))
    }

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_proof_da_reference(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<FullInscriptionMessage> {
        if instructions.len() < MIN_PROOF_DA_REFERENCE_INSTRUCTIONS {
            warn!("Insufficient instructions for proof DA reference");
            return None;
        }

        let l1_batch_reveal_txid =
            match Txid::from_slice(instructions.get(2)?.push_bytes()?.as_bytes()) {
                Ok(txid) => {
                    debug!("Parsed L1 batch reveal txid");
                    txid
                }
                Err(e) => {
                    warn!("Failed to parse L1 batch reveal txid: {}", e);
                    return None;
                }
            };

        let da_identifier = std::str::from_utf8(instructions.get(3)?.push_bytes()?.as_bytes())
            .ok()?
            .to_string();
        debug!("Parsed DA identifier: {}", da_identifier);

        let blob_id = std::str::from_utf8(instructions.get(4)?.push_bytes()?.as_bytes())
            .ok()?
            .to_string();
        debug!("Parsed blob ID: {}", blob_id);

        Some(FullInscriptionMessage::ProofDAReference(ProofDAReference {
            common: common_fields.clone(),
            input: ProofDAReferenceInput {
                l1_batch_reveal_txid,
                da_identifier,
                blob_id,
            },
        }))
    }

    #[instrument(
        skip(self, tx, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_l1_to_l2_message(
        &self,
        tx: &Transaction,
        instructions: &[Instruction],
        common_fields: &CommonFields,
        wallets_opt: Option<&SystemWallets>,
    ) -> Option<FullInscriptionMessage> {
        let wallets = wallets_opt?;

        if instructions.len() < MIN_L1_TO_L2_MESSAGE_INSTRUCTIONS {
            warn!("Insufficient instructions for L1 to L2 message");
            return None;
        }

        let receiver_l2_address = h160_push(instructions.get(2)?)?;
        debug!("Parsed receiver L2 address");

        let l2_contract_address = h160_push(instructions.get(3)?)?;
        debug!("Parsed L2 contract address");

        let call_data = instructions.get(4)?.push_bytes()?.as_bytes().to_vec();
        debug!("Parsed call data, length: {}", call_data.len());

        let amount = tx
            .output
            .iter()
            .find(|output| {
                output.script_pubkey.is_p2tr()
                    && output.script_pubkey == wallets.bridge.script_pubkey()
            })
            .map(|output| output.value)
            .unwrap_or(Amount::ZERO);
        debug!("Parsed amount: {}", amount);

        Some(FullInscriptionMessage::L1ToL2Message(L1ToL2Message {
            common: common_fields.clone(),
            amount,
            input: L1ToL2MessageInput {
                receiver_l2_address,
                l2_contract_address,
                call_data,
            },
            tx_outputs: tx.output.clone(),
        }))
    }

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_system_contract_upgrade_message(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<FullInscriptionMessage> {
        if instructions.len() < MIN_SYSTEM_CONTRACT_UPGRADE_PROPOSAL {
            return None;
        }

        let version = protocol_version_push(instructions.get(2)?)?;
        debug!("Parsed protocol version");

        let bootloader_code_hash = h256_push(instructions.get(3)?)?;
        debug!("Parsed bootloader code hash");

        let default_account_code_hash = h256_push(instructions.get(4)?)?;
        debug!("Parsed default account code hash");

        let recursion_scheduler_level_vk_hash = h256_push(instructions.get(5)?)?;
        debug!("Parsed recursion scheduler level vk hash");

        // After the header: N (address, hash) pairs, one optional terminal OP_ENDIF,
        // and nothing else.
        let tail = instructions.get(6..)?;
        let tail = tail
            .strip_suffix(&[Instruction::Op(OP_ENDIF)])
            .unwrap_or(tail);
        let pairs = tail.chunks_exact(2);
        pairs.remainder().is_empty().then_some(())?;
        let mut system_contracts = Vec::with_capacity(pairs.len());
        for pair in pairs {
            system_contracts.push((h160_push(&pair[0])?, h256_push(&pair[1])?));
        }
        debug!("Parsed system contracts");

        Some(FullInscriptionMessage::SystemContractUpgradeProposal(
            SystemContractUpgradeProposal {
                common: common_fields.clone(),
                input: SystemContractUpgradeProposalInput {
                    version,
                    bootloader_code_hash,
                    default_account_code_hash,
                    evm_emulator_code_hash: None,
                    recursion_scheduler_level_vk_hash,
                    system_contracts,
                },
            },
        ))
    }

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_update_bridge_proposal_message(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<FullInscriptionMessage> {
        if instructions.len() < MIN_UPDATE_BRIDGE_PROPOSAL {
            return None;
        }

        // network unchecked is required to enable serde serialization and deserialization on the library structs
        let verifier_p2wpkh_addresses = instructions[1..instructions.len() - 2]
            .iter()
            .filter_map(|instr| {
                if let Instruction::PushBytes(bytes) = instr {
                    std::str::from_utf8(bytes.as_bytes())
                        .ok()
                        .and_then(|s| s.parse::<Address<NetworkUnchecked>>().ok())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        debug!(
            "Parsed {} verifier addresses",
            verifier_p2wpkh_addresses.len()
        );

        let bridge_musig2_address = instructions.get(instructions.len() - 2).and_then(|instr| {
            if let Instruction::PushBytes(bytes) = instr {
                std::str::from_utf8(bytes.as_bytes())
                    .ok()
                    .and_then(|s| s.parse::<Address<NetworkUnchecked>>().ok())
            } else {
                None
            }
        })?;

        debug!("Parsed bridge address");

        Some(FullInscriptionMessage::UpdateBridgeProposal(
            UpdateBridgeProposal {
                common: common_fields.clone(),
                input: UpdateBridgeProposalInput {
                    bridge_musig2_address,
                    verifier_p2wpkh_addresses,
                },
            },
        ))
    }

    fn parse_inscription_deposit(
        &self,
        tx: &TransactionWithMetadata,
        block_height: u32,
        wallets: &SystemWallets,
    ) -> Option<FullInscriptionMessage> {
        // Try to find any witness data that contains a valid inscription
        for input in tx.tx.input.iter() {
            let witness = &input.witness;
            if witness.len() < MIN_WITNESS_LENGTH {
                continue;
            }

            // Parse signature and control block
            let Ok(signature) = TaprootSignature::from_slice(&witness[0]) else {
                continue;
            };
            let script = ScriptBuf::from_bytes(witness[1].to_vec());
            let Ok(control_block) = ControlBlock::decode(&witness[2]) else {
                continue;
            };

            let instructions: Vec<_> = script.instructions().filter_map(Result::ok).collect();
            let Some(via_index) = find_via_inscription_protocol(&instructions) else {
                continue;
            };
            if !matches!(
                instructions.get(via_index + 1),
                Some(Instruction::PushBytes(bytes))
                    if bytes.as_bytes() == types::L1_TO_L2_MSG.as_bytes()
            ) {
                continue;
            }

            // Try to parse p2wpkh address if possible, but make it optional
            let p2wpkh_address = self.parse_p2wpkh(witness);

            let common_fields = CommonFields {
                schnorr_signature: signature,
                encoded_public_key: PushBytesBuf::from(control_block.internal_key.serialize()),
                block_height,
                tx_id: tx.tx.compute_ntxid().into(),
                p2wpkh_address,
                tx_index: Some(tx.tx_index),
                output_vout: tx.output_vout,
            };

            // Parse L1ToL2Message from instructions
            let Some(message) = self.parse_l1_to_l2_message(
                &tx.tx,
                &instructions[via_index..],
                &common_fields,
                Some(wallets),
            ) else {
                continue;
            };
            return Some(message);
        }

        None
    }

    fn parse_op_return_deposit(
        &self,
        tx: &TransactionWithMetadata,
        block_height: u32,
        bridge_output: &TxOut,
        body: &[u8],
    ) -> Option<FullInscriptionMessage> {
        let receiver_l2_address = EVMAddress::from(<[u8; 20]>::try_from(body.get(..20)?).ok()?);
        let p2wpkh_address = tx
            .tx
            .input
            .first()
            .and_then(|input| self.parse_p2wpkh(&input.witness));
        let common = CommonFields {
            p2wpkh_address,
            tx_index: Some(tx.tx_index),
            output_vout: tx.output_vout,
            ..op_return_common_fields(&tx.tx, block_height)?
        };

        Some(FullInscriptionMessage::L1ToL2Message(L1ToL2Message {
            common,
            amount: bridge_output.value,
            input: L1ToL2MessageInput {
                receiver_l2_address,
                l2_contract_address: EVMAddress::zero(),
                call_data: vec![],
            },
            tx_outputs: tx.tx.output.clone(),
        }))
    }

    fn parse_op_return_withdrawal(
        &self,
        tx: &Transaction,
        block_height: u32,
        wallets: &SystemWallets,
        body: &[u8],
    ) -> Option<FullInscriptionMessage> {
        let version_byte = *body.get(VIA_WI.len())?;
        let version = WithdrawalVersion::try_from(version_byte).ok()?;
        let withdrawal_bytes = body.get(VIA_WI.len() + 1..).filter(|b| !b.is_empty())?;
        let withdrawals_meta = parse_withdrawals(version.clone(), withdrawal_bytes).ok()?;
        let metadata_count = withdrawals_meta.len();
        let mut eligible_outputs = tx.output.iter().filter_map(|output| {
            let receiver = Address::from_script(&output.script_pubkey, self.network).ok()?;
            (receiver != wallets.bridge && output.value != Amount::ZERO)
                .then_some((receiver, output.value))
        });
        let withdrawals: Vec<_> = withdrawals_meta
            .into_iter()
            .zip(eligible_outputs.by_ref())
            .map(|(l2_meta, (receiver, value))| L1Withdrawal {
                l2_meta,
                receiver,
                value,
            })
            .collect();
        if withdrawals.len() != metadata_count || eligible_outputs.next().is_some() {
            return None;
        }
        let input = BridgeWithdrawalInput {
            version,
            v_size: tx.vsize() as i64,
            total_size: tx.total_size() as i64,
            inputs: tx.input.iter().map(|input| input.previous_output).collect(),
            output_amount: tx.output.iter().map(|out| out.value.to_sat()).sum(),
            withdrawals,
        };

        Some(FullInscriptionMessage::BridgeWithdrawal(BridgeWithdrawal {
            common: op_return_common_fields(tx, block_height)?,
            input,
        }))
    }
}

#[instrument(skip(instructions), target = "bitcoin_indexer::parser")]
fn find_via_inscription_protocol(instructions: &[Instruction]) -> Option<usize> {
    let position = instructions.iter().position(|instr| {
        matches!(instr, Instruction::PushBytes(bytes) if bytes.as_bytes() == types::VIA_INSCRIPTION_PROTOCOL.as_bytes())
    });

    if let Some(index) = position {
        debug!("Found VIA inscription protocol at index {}", index);
    } else {
        debug!("VIA inscription protocol not found");
    }

    position
}

pub fn get_eth_address(common_fields: &CommonFields) -> Option<EVMAddress> {
    secp256k1::XOnlyPublicKey::from_slice(common_fields.encoded_public_key.as_bytes())
        .ok()
        .map(|public_key| {
            let pubkey_bytes = public_key.serialize();

            // Take the first 20 bytes of the public key
            let mut address_bytes = [0u8; 20];
            address_bytes.copy_from_slice(&pubkey_bytes[0..20]);

            EVMAddress::from(address_bytes)
        })
}

#[cfg(test)]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use bitcoin::{
        absolute,
        opcodes::{all, OP_FALSE, OP_TRUE},
        script::Builder,
        secp256k1::{Keypair, Secp256k1, SecretKey},
        taproot::{LeafVersion, TaprootBuilder},
        transaction, OutPoint, Sequence, TxIn,
    };
    use zksync_types::{protocol_version::VersionPatch, ProtocolVersionId};

    use super::*;

    #[derive(Clone)]
    enum TestCarrier {
        One(Vec<u8>),
        Two(Vec<u8>, Vec<u8>),
        Empty,
        Bare,
        Raw(Vec<u8>),
        TrailingOpcode(Vec<u8>),
        ExtraPush(Vec<u8>),
    }

    fn push_bytes(bytes: &[u8]) -> PushBytesBuf {
        PushBytesBuf::try_from(bytes.to_vec()).unwrap()
    }

    fn carrier_script(carrier: TestCarrier) -> ScriptBuf {
        match carrier {
            TestCarrier::One(body) => ScriptBuf::new_op_return(push_bytes(&body)),
            TestCarrier::Two(prefix, body) => Builder::new()
                .push_opcode(all::OP_RETURN)
                .push_slice(push_bytes(&prefix))
                .push_slice(push_bytes(&body))
                .into_script(),
            TestCarrier::Empty => Builder::new()
                .push_opcode(all::OP_RETURN)
                .push_slice(PushBytesBuf::new())
                .into_script(),
            TestCarrier::Bare => Builder::new().push_opcode(all::OP_RETURN).into_script(),
            TestCarrier::Raw(bytes) => ScriptBuf::from_bytes(bytes),
            TestCarrier::TrailingOpcode(body) => Builder::new()
                .push_opcode(all::OP_RETURN)
                .push_slice(push_bytes(&body))
                .push_opcode(OP_TRUE)
                .into_script(),
            TestCarrier::ExtraPush(body) => Builder::new()
                .push_opcode(all::OP_RETURN)
                .push_slice(push_bytes(&body))
                .push_slice(push_bytes(&[0xaa]))
                .push_slice(push_bytes(&[0xbb]))
                .into_script(),
        }
    }

    fn test_transaction(outputs: Vec<TxOut>) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![test_input(Witness::new())],
            output: outputs,
        }
    }

    fn test_input(witness: Witness) -> TxIn {
        TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness,
        }
    }

    fn p2wpkh_witness(secret_key_bytes: [u8; 32]) -> Witness {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(&secret_key_bytes).unwrap();
        let public_key = bitcoin::PublicKey::new(bitcoin::secp256k1::PublicKey::from_secret_key(
            &secp,
            &secret_key,
        ));
        Witness::from_slice(&[Vec::new(), public_key.to_bytes()])
    }

    fn op_return_output(script_pubkey: ScriptBuf) -> TxOut {
        TxOut {
            value: Amount::ZERO,
            script_pubkey,
        }
    }

    fn bridge_output(wallets: &SystemWallets) -> TxOut {
        TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: wallets.bridge.script_pubkey(),
        }
    }

    fn bridge_transaction(
        carriers: impl IntoIterator<Item = TestCarrier>,
    ) -> TransactionWithMetadata {
        let wallets = system_wallets();
        let mut outputs = vec![bridge_output(&wallets)];
        outputs.extend(
            carriers
                .into_iter()
                .map(carrier_script)
                .map(op_return_output),
        );
        TransactionWithMetadata::new(test_transaction(outputs), 7)
    }

    fn parsed_deposit_receiver(messages: &[FullInscriptionMessage]) -> Option<EVMAddress> {
        match messages {
            [FullInscriptionMessage::L1ToL2Message(message)] => {
                Some(message.input.receiver_l2_address)
            }
            _ => None,
        }
    }

    fn inscription_witness(receiver: EVMAddress) -> Witness {
        inscription_witness_with_fields(
            &types::L1_TO_L2_MSG,
            &[
                receiver.as_bytes().to_vec(),
                EVMAddress::zero().as_bytes().to_vec(),
                Vec::new(),
            ],
        )
    }

    fn inscription_witness_with_fields(message_type: &PushBytesBuf, fields: &[Vec<u8>]) -> Witness {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(&[1; 32]).unwrap();
        let keypair = Keypair::from_secret_key(&secp, &secret_key);
        let (internal_key, _) = keypair.x_only_public_key();
        let mut script = Builder::new()
            .push_slice(push_bytes(&internal_key.serialize()))
            .push_opcode(all::OP_CHECKSIG)
            .push_opcode(OP_FALSE)
            .push_opcode(all::OP_IF)
            .push_slice(push_bytes(types::VIA_INSCRIPTION_PROTOCOL.as_bytes()))
            .push_slice(message_type);
        for field in fields {
            script = script.push_slice(push_bytes(field));
        }
        let script = script.push_opcode(all::OP_ENDIF).into_script();
        let spend_info = TaprootBuilder::new()
            .add_leaf(0, script.clone())
            .unwrap()
            .finalize(&secp, internal_key)
            .unwrap();
        let control_block = spend_info
            .control_block(&(script.clone(), LeafVersion::TapScript))
            .unwrap();

        Witness::from_slice(&[
            TaprootSignature::from_slice(&[0; 64]).unwrap().to_vec(),
            script.into_bytes(),
            control_block.serialize(),
        ])
    }

    fn parse_system_witness(witness: Witness) -> Vec<FullInscriptionMessage> {
        let mut tx = test_transaction(Vec::new());
        tx.input = vec![test_input(witness), test_input(p2wpkh_witness([2; 32]))];
        MessageParser::new(Network::Regtest).parse_system_transaction(
            &tx,
            42,
            Some(&system_wallets()),
        )
    }

    fn packed_version() -> ProtocolSemanticVersion {
        ProtocolSemanticVersion::new(ProtocolVersionId::Version28, VersionPatch(1))
    }

    fn packed_version_bytes() -> Vec<u8> {
        let mut canonical = [0; 32];
        packed_version().pack().to_big_endian(&mut canonical);
        canonical
            .into_iter()
            .skip_while(|byte| *byte == 0)
            .collect()
    }

    fn system_bootstrapping_fields() -> Vec<Vec<u8>> {
        let wallets = system_wallets();
        vec![
            42_u32.to_be_bytes().to_vec(),
            packed_version_bytes(),
            vec![0x11; 32],
            vec![0x22; 32],
            vec![0x33; 32],
            vec![0x44; 32],
            wallets.governance.to_string().into_bytes(),
            wallets.sequencer.to_string().into_bytes(),
            wallets.bridge.to_string().into_bytes(),
        ]
    }

    fn l1_batch_da_reference_fields() -> Vec<Vec<u8>> {
        vec![
            vec![0x11; 32],
            7_u32.to_be_bytes().to_vec(),
            b"da".to_vec(),
            b"blob".to_vec(),
            vec![0x22; 32],
        ]
    }

    fn system_contract_upgrade_fields() -> Vec<Vec<u8>> {
        vec![
            packed_version_bytes(),
            vec![0x11; 32],
            vec![0x22; 32],
            vec![0x33; 32],
        ]
    }

    fn replace_witness_item(witness: &Witness, index: usize, replacement: Vec<u8>) -> Witness {
        let mut items: Vec<_> = witness.iter().map(|item| item.to_vec()).collect();
        items[index] = replacement;
        Witness::from_slice(&items)
    }

    fn candidate_inscription_script(message_type: &PushBytesBuf, fields: &[&[u8]]) -> ScriptBuf {
        let mut script = Builder::new()
            .push_slice(push_bytes(types::VIA_INSCRIPTION_PROTOCOL.as_bytes()))
            .push_slice(message_type);
        for field in fields {
            script = script.push_slice(push_bytes(field));
        }
        script.into_script()
    }

    fn system_wallets() -> SystemWallets {
        SystemWallets {
            sequencer: Address::from_str("bcrt1qw2mvkvm6alfhe86yf328kgvr7mupdx4vln7kpv")
                .unwrap()
                .assume_checked(),
            bridge: Address::from_str(
                "bcrt1pcx974cg2w66cqhx67zadf85t8k4sd2wp68l8x8agd3aj4tuegsgsz97amg",
            )
            .unwrap()
            .assume_checked(),
            governance: Address::from_str(
                "bcrt1q92gkfme6k9dkpagrkwt76etkaq29hvf02w5m38f6shs4ddpw7hzqp347zm",
            )
            .unwrap()
            .assume_checked(),
            verifiers: vec![],
        }
    }

    #[test]
    fn active_op_return_taxonomy_preserves_match_relations() {
        for &(kind, prefix) in ACTIVE_OP_RETURN_PREFIXES {
            assert_eq!(active_kind_from_leading_prefix(prefix), Some(kind));
            assert_eq!(active_kind_from_exact_prefix(prefix), Some(kind));

            let mut extended = prefix.to_vec();
            extended.push(0xff);
            assert_eq!(active_kind_from_leading_prefix(&extended), Some(kind));
            assert_eq!(active_kind_from_exact_prefix(&extended), None);
        }

        assert_eq!(
            active_kind_from_leading_prefix(OP_RETURN_RETIRED_WITHDRAW_PREFIX),
            None
        );
        assert_eq!(
            active_kind_from_exact_prefix(OP_RETURN_RETIRED_WITHDRAW_PREFIX),
            None
        );
    }

    #[test]
    fn continues_after_short_or_long_l1_to_l2_fixed_pushes() {
        let wallets = system_wallets();
        let valid_receiver = EVMAddress::repeat_byte(0x81);
        let cases = [
            (vec![0x11; 19], vec![0x22; 20]),
            (vec![0x11; 21], vec![0x22; 20]),
            (vec![0x11; 20], vec![0x22; 19]),
            (vec![0x11; 20], vec![0x22; 21]),
        ];

        for (receiver, contract) in cases {
            let invalid_witness = inscription_witness_with_fields(
                &types::L1_TO_L2_MSG,
                &[receiver, contract, Vec::new()],
            );
            let mut tx = bridge_transaction([]);
            tx.tx.input = vec![
                test_input(invalid_witness),
                test_input(inscription_witness(valid_receiver)),
            ];

            let result = catch_unwind(AssertUnwindSafe(|| {
                MessageParser::new(Network::Regtest).parse_bridge_transaction(&mut tx, 42, &wallets)
            }));
            assert!(matches!(
                result,
                Ok(messages) if parsed_deposit_receiver(&messages) == Some(valid_receiver)
            ));
        }
    }

    #[test]
    fn rejects_inexact_system_fixed_pushes_without_panicking() {
        for field_index in 2..=5 {
            for width in [31, 33] {
                let mut fields = system_bootstrapping_fields();
                fields[field_index] = vec![0x55; width];
                let witness =
                    inscription_witness_with_fields(&types::SYSTEM_BOOTSTRAPPING_MSG, &fields);
                let result = catch_unwind(AssertUnwindSafe(|| parse_system_witness(witness)));
                assert!(matches!(result, Ok(messages) if messages.is_empty()));
            }
        }

        for field_index in [0, 4] {
            for width in [31, 33] {
                let mut fields = l1_batch_da_reference_fields();
                fields[field_index] = vec![0x66; width];
                let witness =
                    inscription_witness_with_fields(&types::L1_BATCH_DA_REFERENCE_MSG, &fields);
                let result = catch_unwind(AssertUnwindSafe(|| parse_system_witness(witness)));
                assert!(matches!(result, Ok(messages) if messages.is_empty()));
            }
        }
    }

    #[test]
    fn rejects_oversized_packed_version_pushes_without_panicking() {
        let mut bootstrapping_fields = system_bootstrapping_fields();
        bootstrapping_fields[1] = vec![0x77; 33];
        let mut proposal_fields = system_contract_upgrade_fields();
        proposal_fields[0] = vec![0x77; 33];

        for (message_type, fields) in [
            (&*types::SYSTEM_BOOTSTRAPPING_MSG, bootstrapping_fields),
            (&*types::SYSTEM_CONTRACT_UPGRADE_MSG, proposal_fields),
        ] {
            let witness = inscription_witness_with_fields(message_type, &fields);
            let result = catch_unwind(AssertUnwindSafe(|| parse_system_witness(witness)));
            assert!(matches!(result, Ok(messages) if messages.is_empty()));
        }

        let version_bytes = packed_version_bytes();
        assert!(version_bytes.len() < 32);
        let witness = inscription_witness_with_fields(
            &types::SYSTEM_BOOTSTRAPPING_MSG,
            &system_bootstrapping_fields(),
        );
        let messages = parse_system_witness(witness);
        assert!(matches!(
            messages.as_slice(),
            [FullInscriptionMessage::SystemBootstrapping(message)]
                if message.input.protocol_version == packed_version()
        ));
    }

    #[test]
    fn decodes_push_boundaries_and_non_minimal_pushes() {
        for len in [73, 74, 75, 76, 77] {
            let payload: Vec<_> = (0..len).map(|byte| byte as u8).collect();
            let tx = test_transaction(vec![op_return_output(carrier_script(TestCarrier::One(
                payload.clone(),
            )))]);
            assert!(matches!(
                first_op_return_carrier(&tx),
                Some(OpReturnCarrier::One(body)) if body == payload
            ));
        }

        let payload: Vec<_> = (0..20).map(|byte| byte as u8).collect();
        let mut non_minimal = vec![all::OP_RETURN.to_u8(), 0x4c, 20];
        non_minimal.extend_from_slice(&payload);
        let tx = test_transaction(vec![op_return_output(carrier_script(TestCarrier::Raw(
            non_minimal,
        )))]);
        assert!(matches!(
            first_op_return_carrier(&tx),
            Some(OpReturnCarrier::One(body)) if body == payload
        ));

        let tx = test_transaction(vec![op_return_output(carrier_script(TestCarrier::Empty))]);
        assert!(matches!(
            first_op_return_carrier(&tx),
            Some(OpReturnCarrier::One([]))
        ));
    }

    #[test]
    fn rejects_carriers_with_invalid_arity_or_instructions() {
        let payload = vec![0x22; 20];
        let mut malformed = vec![all::OP_RETURN.to_u8(), 20];
        malformed.extend([0x11; 19]);
        let carriers = [
            TestCarrier::Bare,
            TestCarrier::Raw(malformed),
            TestCarrier::TrailingOpcode(payload.clone()),
            TestCarrier::ExtraPush(payload),
        ];

        for carrier in carriers {
            let tx = test_transaction(vec![op_return_output(carrier_script(carrier))]);
            assert!(first_op_return_carrier(&tx).is_none());
        }
    }

    #[test]
    fn characterizes_one_push_deposit_receiver_windows() {
        let wallets = system_wallets();
        let mut parser = MessageParser::new(Network::Regtest);
        let sender_witness = p2wpkh_witness([2; 32]);
        let sender_address = parser.parse_p2wpkh(&sender_witness).unwrap();
        let cases = [20, 74, 75];

        for len in cases {
            let payload: Vec<_> = (0..len).map(|byte| byte as u8).collect();
            let mut tx = bridge_transaction([TestCarrier::One(payload.clone())]);
            tx.tx.input[0].witness = sender_witness.clone();
            let messages = parser.parse_bridge_transaction(&mut tx, 42, &wallets);
            let [FullInscriptionMessage::L1ToL2Message(message)] = messages.as_slice() else {
                panic!("expected one deposit for len={len}");
            };
            assert_eq!(
                message.input.receiver_l2_address,
                EVMAddress::from_slice(&payload[..20]),
                "len={len}"
            );
            assert_eq!(
                message.common.p2wpkh_address.as_ref(),
                Some(&sender_address),
                "len={len}"
            );
            assert_eq!(message.common.tx_index, Some(7), "len={len}");
            assert_eq!(message.common.output_vout, Some(0), "len={len}");
        }

        let payload: Vec<_> = (0..20).map(|byte| byte as u8).collect();
        let mut non_minimal = vec![all::OP_RETURN.to_u8(), 0x4c, 20];
        non_minimal.extend_from_slice(&payload);
        let mut tx = bridge_transaction([TestCarrier::Raw(non_minimal)]);
        let messages =
            MessageParser::new(Network::Regtest).parse_bridge_transaction(&mut tx, 42, &wallets);
        assert_eq!(
            parsed_deposit_receiver(&messages),
            Some(EVMAddress::from_slice(&payload))
        );
    }

    #[test]
    fn characterizes_two_push_governance_carriers() {
        enum Expected {
            Upgrade,
            Bridge,
            Sequencer,
            Governance,
        }

        let wallets = system_wallets();
        let mut txid_body = vec![0x31; 32];
        txid_body.push(0xff);
        let cases = [
            (
                OP_RETURN_UPGRADE_PROTOCOL_PREFIX,
                txid_body.clone(),
                Expected::Upgrade,
            ),
            (
                OP_RETURN_UPDATE_BRIDGE_PREFIX,
                vec![0x42; 32],
                Expected::Bridge,
            ),
            (
                OP_RETURN_UPDATE_SEQUENCER_PREFIX,
                wallets.sequencer.to_string().into_bytes(),
                Expected::Sequencer,
            ),
            (
                OP_RETURN_UPDATE_GOVERNANCE_PREFIX,
                wallets.governance.to_string().into_bytes(),
                Expected::Governance,
            ),
        ];

        for (prefix, body, expected) in cases {
            let tx = TransactionWithMetadata::new(
                test_transaction(vec![op_return_output(carrier_script(TestCarrier::Two(
                    prefix.to_vec(),
                    body,
                )))]),
                0,
            );
            let messages =
                MessageParser::new(Network::Regtest).parse_protocol_upgrade_transactions(&tx, 42);
            assert_eq!(messages.len(), 1);
            match (&messages[0], expected) {
                (FullInscriptionMessage::SystemContractUpgrade(message), Expected::Upgrade) => {
                    assert_eq!(
                        message.input.proposal_tx_id,
                        Txid::from_slice(&[0x31; 32]).unwrap()
                    );
                }
                (FullInscriptionMessage::UpdateBridge(message), Expected::Bridge) => {
                    assert_eq!(
                        message.input.proposal_tx_id,
                        Txid::from_slice(&[0x42; 32]).unwrap()
                    );
                }
                (FullInscriptionMessage::UpdateSequencer(message), Expected::Sequencer) => {
                    assert_eq!(
                        message.input.address.clone().assume_checked(),
                        wallets.sequencer
                    );
                }
                (FullInscriptionMessage::UpdateGovernance(message), Expected::Governance) => {
                    assert_eq!(
                        message.input.address.clone().assume_checked(),
                        wallets.governance
                    );
                }
                _ => panic!("unexpected governance message"),
            }
        }
    }

    #[test]
    fn rejects_malformed_governance_bodies_without_panicking() {
        let cases = [
            (VIA_WI, vec![0]),
            (OP_RETURN_UPGRADE_PROTOCOL_PREFIX, vec![0x11; 31]),
            (OP_RETURN_UPDATE_BRIDGE_PREFIX, Vec::new()),
            (OP_RETURN_UPDATE_SEQUENCER_PREFIX, Vec::new()),
            (OP_RETURN_UPDATE_SEQUENCER_PREFIX, vec![0xff]),
            (
                OP_RETURN_UPDATE_SEQUENCER_PREFIX,
                b"not-an-address".to_vec(),
            ),
            (OP_RETURN_UPDATE_GOVERNANCE_PREFIX, Vec::new()),
        ];

        for (prefix, body) in cases {
            let tx = TransactionWithMetadata::new(
                test_transaction(vec![op_return_output(carrier_script(TestCarrier::Two(
                    prefix.to_vec(),
                    body,
                )))]),
                0,
            );
            let result = catch_unwind(AssertUnwindSafe(|| {
                MessageParser::new(Network::Regtest).parse_protocol_upgrade_transactions(&tx, 42)
            }));
            assert!(matches!(result, Ok(messages) if messages.is_empty()));
        }

        let mut inexact_prefix = OP_RETURN_UPGRADE_PROTOCOL_PREFIX.to_vec();
        inexact_prefix.push(0);
        let tx = TransactionWithMetadata::new(
            test_transaction(vec![op_return_output(carrier_script(TestCarrier::Two(
                inexact_prefix,
                vec![0x11; 32],
            )))]),
            0,
        );
        assert!(MessageParser::new(Network::Regtest)
            .parse_protocol_upgrade_transactions(&tx, 42)
            .is_empty());
    }

    #[test]
    fn rejects_empty_via_wi_withdrawal() {
        let wallets = system_wallets();
        let mut payload = VIA_WI.to_vec();
        payload.push(WithdrawalVersion::Version0 as u8);
        assert_eq!(payload.len(), 7);
        let mut tx = bridge_transaction([TestCarrier::One(payload)]);

        let result = catch_unwind(AssertUnwindSafe(|| {
            MessageParser::new(Network::Regtest).parse_bridge_transaction(&mut tx, 42, &wallets)
        }));
        assert!(matches!(result, Ok(messages) if messages.is_empty()));
    }

    #[test]
    fn characterizes_withdrawal_carrier_lengths() {
        let wallets = system_wallets();

        for record_count in [6, 7] {
            let mut payload = VIA_WI.to_vec();
            payload.push(0);
            payload.extend((0..record_count * 10).map(|byte| byte as u8));

            let mut outputs: Vec<_> = (0..record_count)
                .map(|_| TxOut {
                    value: Amount::from_sat(1_000),
                    script_pubkey: wallets.sequencer.script_pubkey(),
                })
                .collect();
            outputs.push(bridge_output(&wallets));
            outputs.push(op_return_output(carrier_script(TestCarrier::One(
                payload.clone(),
            ))));
            let mut tx = TransactionWithMetadata::new(test_transaction(outputs), 0);

            let messages = MessageParser::new(Network::Regtest)
                .parse_bridge_transaction(&mut tx, 42, &wallets);
            assert_eq!(payload.len(), 7 + 10 * record_count);
            assert!(matches!(
                messages.as_slice(),
                [FullInscriptionMessage::BridgeWithdrawal(message)]
                    if message.input.withdrawals.len() == record_count
            ));
        }
    }

    #[test]
    fn maps_withdrawal_metadata_to_eligible_outputs_in_lockstep() {
        let wallets = system_wallets();
        let mut payload = VIA_WI.to_vec();
        payload.push(0);
        payload.extend([0x11; 10]);
        payload.extend([0x22; 10]);
        let mut tx = TransactionWithMetadata::new(
            test_transaction(vec![
                op_return_output(carrier_script(TestCarrier::One(payload))),
                bridge_output(&wallets),
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: wallets.sequencer.script_pubkey(),
                },
                TxOut {
                    value: Amount::from_sat(500),
                    script_pubkey: Builder::new().push_opcode(OP_TRUE).into_script(),
                },
                TxOut {
                    value: Amount::from_sat(1_000),
                    script_pubkey: wallets.sequencer.script_pubkey(),
                },
                TxOut {
                    value: Amount::from_sat(2_000),
                    script_pubkey: wallets.governance.script_pubkey(),
                },
            ]),
            0,
        );

        let messages =
            MessageParser::new(Network::Regtest).parse_bridge_transaction(&mut tx, 42, &wallets);
        assert!(matches!(
            messages.as_slice(),
            [FullInscriptionMessage::BridgeWithdrawal(message)]
                if message.input.withdrawals.len() == 2
                    && message.input.withdrawals[0].l2_meta.l2_id == hex::encode([0x11; 10])
                    && message.input.withdrawals[0].receiver == wallets.sequencer
                    && message.input.withdrawals[1].l2_meta.l2_id == hex::encode([0x22; 10])
                    && message.input.withdrawals[1].receiver == wallets.governance
        ));
    }

    #[test]
    fn rejects_withdrawal_metadata_count_mismatches_without_panicking() {
        let wallets = system_wallets();

        for (metadata_count, payout_count) in [(1, 2), (2, 1)] {
            let mut payload = VIA_WI.to_vec();
            payload.push(0);
            payload.extend(vec![0x44; metadata_count * 10]);
            let mut outputs = vec![op_return_output(carrier_script(TestCarrier::One(payload)))];
            outputs.extend((0..payout_count).map(|_| TxOut {
                value: Amount::from_sat(1_000),
                script_pubkey: wallets.sequencer.script_pubkey(),
            }));
            outputs.push(bridge_output(&wallets));
            let mut tx = TransactionWithMetadata::new(test_transaction(outputs), 0);

            let result = catch_unwind(AssertUnwindSafe(|| {
                MessageParser::new(Network::Regtest).parse_bridge_transaction(&mut tx, 42, &wallets)
            }));
            assert!(matches!(result, Ok(messages) if messages.is_empty()));
        }
    }

    #[test]
    fn rejects_reserved_and_irregular_carriers() {
        let wallets = system_wallets();
        let deposit = vec![0x25; 20];
        let reserved = [
            VIA_WI.to_vec(),
            OP_RETURN_UPGRADE_PROTOCOL_PREFIX.to_vec(),
            OP_RETURN_UPDATE_SEQUENCER_PREFIX.to_vec(),
            OP_RETURN_UPDATE_BRIDGE_PREFIX.to_vec(),
            OP_RETURN_UPDATE_GOVERNANCE_PREFIX.to_vec(),
            OP_RETURN_RETIRED_WITHDRAW_PREFIX.to_vec(),
        ];
        for mut body in reserved {
            body.extend([0x33; 32]);
            let mut tx = bridge_transaction([TestCarrier::One(body)]);
            let messages = MessageParser::new(Network::Regtest)
                .parse_bridge_transaction(&mut tx, 42, &wallets);
            assert!(messages.is_empty());
        }

        let rejected = [
            TestCarrier::TrailingOpcode(deposit.clone()),
            TestCarrier::ExtraPush(deposit.clone()),
        ];
        for carrier in rejected {
            let mut tx = bridge_transaction([carrier]);
            let messages = MessageParser::new(Network::Regtest)
                .parse_bridge_transaction(&mut tx, 42, &wallets);
            assert!(messages.is_empty());
        }

        let malformed = {
            let mut bytes = vec![all::OP_RETURN.to_u8(), 20];
            bytes.extend([0x11; 19]);
            TestCarrier::Raw(bytes)
        };
        let mut bad_version = VIA_WI.to_vec();
        bad_version.push(1);
        let mut bad_record_length = VIA_WI.to_vec();
        bad_record_length.extend([0, 0x11]);
        for carrier in [
            TestCarrier::Bare,
            TestCarrier::Empty,
            malformed,
            TestCarrier::One(vec![0x11; 19]),
            TestCarrier::One(VIA_WI.to_vec()),
            TestCarrier::One(bad_version),
            TestCarrier::One(bad_record_length),
        ] {
            let mut tx = bridge_transaction([carrier]);
            let result = catch_unwind(AssertUnwindSafe(|| {
                MessageParser::new(Network::Regtest).parse_bridge_transaction(&mut tx, 42, &wallets)
            }));
            assert!(matches!(result, Ok(messages) if messages.is_empty()));
        }
    }

    #[test]
    fn characterizes_first_op_return_selection() {
        let wallets = system_wallets();
        let valid = vec![0x39; 20];
        let mut malformed = vec![all::OP_RETURN.to_u8(), 20];
        malformed.extend([0x11; 19]);

        let mut malformed_first =
            bridge_transaction([TestCarrier::Raw(malformed), TestCarrier::One(valid.clone())]);
        let result = catch_unwind(AssertUnwindSafe(|| {
            MessageParser::new(Network::Regtest).parse_bridge_transaction(
                &mut malformed_first,
                42,
                &wallets,
            )
        }));
        assert!(matches!(result, Ok(messages) if messages.is_empty()));

        let mut valid_first = bridge_transaction([
            TestCarrier::One(valid.clone()),
            TestCarrier::ExtraPush(vec![0x77; 20]),
        ]);
        let messages = MessageParser::new(Network::Regtest).parse_bridge_transaction(
            &mut valid_first,
            42,
            &wallets,
        );
        assert_eq!(
            parsed_deposit_receiver(&messages),
            Some(EVMAddress::from_slice(&valid))
        );
    }

    #[test]
    fn finds_later_deposit_after_each_invalid_inscription_candidate() {
        let wallets = system_wallets();
        let valid_receiver = EVMAddress::repeat_byte(0x81);
        let other_message_receiver = EVMAddress::repeat_byte(0x72);
        let template = inscription_witness(EVMAddress::repeat_byte(0x70));
        let non_via_script = Builder::new()
            .push_slice(push_bytes(b"not-via"))
            .into_script();
        let invalid_candidates = [
            replace_witness_item(&template, 0, vec![0]),
            replace_witness_item(&template, 2, vec![0]),
            replace_witness_item(&template, 1, non_via_script.into_bytes()),
            replace_witness_item(
                &template,
                1,
                candidate_inscription_script(
                    &types::VALIDATOR_ATTESTATION_MSG,
                    &[
                        other_message_receiver.as_bytes(),
                        EVMAddress::zero().as_bytes(),
                        &[],
                    ],
                )
                .into_bytes(),
            ),
            replace_witness_item(
                &template,
                1,
                candidate_inscription_script(&types::L1_TO_L2_MSG, &[]).into_bytes(),
            ),
        ];

        for invalid_candidate in invalid_candidates {
            let mut tx = bridge_transaction([]);
            tx.tx.input = vec![
                test_input(invalid_candidate),
                test_input(inscription_witness(valid_receiver)),
            ];
            let messages = MessageParser::new(Network::Regtest)
                .parse_bridge_transaction(&mut tx, 42, &wallets);
            assert_eq!(parsed_deposit_receiver(&messages), Some(valid_receiver));
        }
    }

    #[test]
    fn system_transaction_keeps_single_da_reveal_multiplicity() {
        let template = inscription_witness(EVMAddress::repeat_byte(0x91));
        let l1_batch_hash = H256::repeat_byte(0x11);
        let l1_batch_index = 7_u32.to_be_bytes();
        let prev_l1_batch_hash = H256::repeat_byte(0x22);
        let reveal_script = candidate_inscription_script(
            &types::L1_BATCH_DA_REFERENCE_MSG,
            &[
                l1_batch_hash.as_bytes(),
                &l1_batch_index,
                b"da",
                b"blob",
                prev_l1_batch_hash.as_bytes(),
            ],
        );
        let reveal_witness = replace_witness_item(&template, 1, reveal_script.into_bytes());
        let sender_witness = p2wpkh_witness([2; 32]);
        let mut tx = test_transaction(Vec::new());
        tx.input = vec![test_input(reveal_witness), test_input(sender_witness)];

        let messages = MessageParser::new(Network::Regtest).parse_system_transaction(
            &tx,
            42,
            Some(&system_wallets()),
        );
        assert!(matches!(
            messages.as_slice(),
            [FullInscriptionMessage::L1BatchDAReference(message)]
                if message.input.l1_batch_hash == l1_batch_hash
                    && message.input.l1_batch_index == L1BatchNumber(7)
        ));
    }

    #[test]
    fn witness_deposit_suppresses_conflicting_op_return_deposit() {
        let wallets = system_wallets();
        let witness_receiver = EVMAddress::repeat_byte(0x51);
        let op_return_receiver = EVMAddress::repeat_byte(0x62);

        let mut tx = bridge_transaction([TestCarrier::One(op_return_receiver.as_bytes().to_vec())]);
        tx.tx.input[0].witness = inscription_witness(witness_receiver);
        let messages =
            MessageParser::new(Network::Regtest).parse_bridge_transaction(&mut tx, 42, &wallets);

        let [FullInscriptionMessage::L1ToL2Message(message)] = messages.as_slice() else {
            panic!("expected one witness deposit");
        };
        assert_eq!(message.input.receiver_l2_address, witness_receiver);
        assert!(!message.common.encoded_public_key.is_empty());
    }

    #[test]
    fn witness_deposit_preserves_via_wi_withdrawal_order() {
        let wallets = system_wallets();
        let witness_receiver = EVMAddress::repeat_byte(0x51);

        let mut withdrawal_payload = VIA_WI.to_vec();
        withdrawal_payload.push(0);
        withdrawal_payload.extend([0x71; 10]);
        let mut withdrawal_tx = TransactionWithMetadata::new(
            test_transaction(vec![
                TxOut {
                    value: Amount::from_sat(1_000),
                    script_pubkey: wallets.sequencer.script_pubkey(),
                },
                bridge_output(&wallets),
                op_return_output(carrier_script(TestCarrier::One(withdrawal_payload))),
            ]),
            0,
        );
        withdrawal_tx.tx.input[0].witness = inscription_witness(witness_receiver);
        let messages = MessageParser::new(Network::Regtest).parse_bridge_transaction(
            &mut withdrawal_tx,
            42,
            &wallets,
        );
        assert!(matches!(
            messages.as_slice(),
            [
                FullInscriptionMessage::L1ToL2Message(first),
                FullInscriptionMessage::BridgeWithdrawal(_),
            ] if first.input.receiver_l2_address == witness_receiver
        ));
    }

    #[test]
    fn op_return_deposit_is_used_without_a_valid_witness_deposit() {
        let wallets = system_wallets();
        let op_return_receiver = EVMAddress::repeat_byte(0x62);
        let invalid_witness = inscription_witness_with_fields(
            &types::L1_TO_L2_MSG,
            &[
                vec![0x51; 19],
                EVMAddress::zero().as_bytes().to_vec(),
                Vec::new(),
            ],
        );

        for witness in [Witness::new(), invalid_witness] {
            let mut tx =
                bridge_transaction([TestCarrier::One(op_return_receiver.as_bytes().to_vec())]);
            tx.tx.input[0].witness = witness;
            let messages = MessageParser::new(Network::Regtest)
                .parse_bridge_transaction(&mut tx, 42, &wallets);

            let [FullInscriptionMessage::L1ToL2Message(message)] = messages.as_slice() else {
                panic!("expected one OP_RETURN deposit");
            };
            assert_eq!(message.input.receiver_l2_address, op_return_receiver);
            assert!(message.common.encoded_public_key.is_empty());
        }
    }

    #[test]
    fn matching_dual_deposits_emit_once() {
        let wallets = system_wallets();
        let receiver = EVMAddress::repeat_byte(0x51);
        let mut tx = bridge_transaction([TestCarrier::One(receiver.as_bytes().to_vec())]);
        tx.tx.input[0].witness = inscription_witness(receiver);

        let messages =
            MessageParser::new(Network::Regtest).parse_bridge_transaction(&mut tx, 42, &wallets);
        let [FullInscriptionMessage::L1ToL2Message(message)] = messages.as_slice() else {
            panic!("expected one witness deposit");
        };
        assert_eq!(message.input.receiver_l2_address, receiver);
        assert!(!message.common.encoded_public_key.is_empty());
    }
}
