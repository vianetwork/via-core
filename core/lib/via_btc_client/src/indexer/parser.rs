use bitcoin::{
    address::NetworkUnchecked,
    hashes::Hash,
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
    indexer::withdrawal::{parse_withdrawals, L1Withdrawal, WithdrawalVersion},
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

const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_WI";
const OP_RETURN_UPGRADE_PROTOCOL_PREFIX: &[u8] = b"VIA_PROTOCOL:UPGRADE";
const OP_RETURN_UPDATE_SEQUENCER_PREFIX: &[u8] = b"VIA_PROTOCOL:SEQ";
const OP_RETURN_UPDATE_BRIDGE_PREFIX: &[u8] = b"VIA_PROTOCOL:BRI";
const OP_RETURN_UPDATE_GOVERNANCE_PREFIX: &[u8] = b"VIA_PROTOCOL:GOV";

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

#[derive(Debug)]
pub(super) enum CarrierParse<T> {
    Irrelevant,
    Malformed(String),
    Unsupported(Vec<u8>),
    ContextRequired,
    Valid(T),
}

#[derive(Clone, Debug)]
pub(super) struct CarrierMetadata {
    pub block_height: u32,
    pub signer: Option<Address>,
    pub tx_index: Option<usize>,
    pub output_vout: Option<usize>,
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
        let mut messages = Vec::new();

        if let Some(update_sequencer) = self.parse_op_return_update_governance(&tx.tx, block_height)
        {
            messages.push(update_sequencer);
        }

        if let Some(upgrade_protocol) = self.parse_op_return_protocol_upgrade(&tx.tx, block_height)
        {
            messages.push(upgrade_protocol);
        }

        if let Some(update_bridge) = self.parse_op_return_update_bridge(&tx.tx, block_height) {
            messages.push(update_bridge);
        }

        if let Some(update_sequencer) = self.parse_op_return_update_sequencer(&tx.tx, block_height)
        {
            messages.push(update_sequencer);
        }

        messages
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

        // Try to parse as inscription-based deposit first
        if let Some(inscription_message) = self.parse_inscription_deposit(tx, block_height, wallets)
        {
            messages.push(inscription_message);
        }

        // If not an inscription, try to parse as OP_RETURN based deposit
        if let Some(op_return_message) =
            self.parse_op_return_deposit(tx, block_height, bridge_output)
        {
            messages.push(op_return_message);
        }

        // Try to parse withdrawals processed by the bridge address.
        if let Some(bridge_withdrawals) =
            self.parse_op_return_withdrawal(&tx.tx, block_height, wallets)
        {
            messages.push(bridge_withdrawals);
        }

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
        let metadata = CarrierMetadata {
            block_height,
            signer: Some(address),
            tx_index: None,
            output_vout: None,
        };
        match self.parse_system_carrier(input, tx, metadata, wallets) {
            CarrierParse::Valid(message) => Some(message),
            CarrierParse::Irrelevant
            | CarrierParse::Malformed(_)
            | CarrierParse::Unsupported(_)
            | CarrierParse::ContextRequired => None,
        }
    }

    pub(super) fn parse_system_carrier(
        &mut self,
        input: &bitcoin::TxIn,
        tx: &Transaction,
        metadata: CarrierMetadata,
        wallets: Option<&SystemWallets>,
    ) -> CarrierParse<FullInscriptionMessage> {
        let witness = &input.witness;
        if witness.len() < MIN_WITNESS_LENGTH {
            return CarrierParse::Irrelevant;
        }
        let script = ScriptBuf::from_bytes(witness[1].to_vec());
        if !script
            .as_bytes()
            .windows(types::VIA_INSCRIPTION_PROTOCOL.len())
            .any(|window| window == types::VIA_INSCRIPTION_PROTOCOL.as_bytes())
        {
            return CarrierParse::Irrelevant;
        }
        let instructions = match script.instructions().collect::<Result<Vec<_>, _>>() {
            Ok(instructions) => instructions,
            Err(err) => {
                return CarrierParse::Malformed(format!("invalid inscription script: {err}"))
            }
        };
        let Some(via_index) = find_via_inscription_protocol(&instructions) else {
            return CarrierParse::Irrelevant;
        };
        let signature = match TaprootSignature::from_slice(&witness[0]) {
            Ok(signature) => signature,
            Err(err) => {
                return CarrierParse::Malformed(format!("invalid Taproot signature: {err}"))
            }
        };
        let control_block = match ControlBlock::decode(&witness[2]) {
            Ok(control_block) => control_block,
            Err(err) => return CarrierParse::Malformed(format!("invalid control block: {err}")),
        };
        let message_instructions = &instructions[via_index..];
        let message_type = match message_instructions.get(1) {
            Some(Instruction::PushBytes(bytes)) => bytes.as_bytes(),
            Some(Instruction::Op(_)) => {
                return CarrierParse::Malformed("message type is not push data".into())
            }
            None => return CarrierParse::Malformed("missing message type".into()),
        };
        if !is_known_system_message_type(message_type) {
            return CarrierParse::Unsupported(message_type.to_vec());
        }
        if message_type == types::L1_TO_L2_MSG.as_bytes() && wallets.is_none() {
            return CarrierParse::ContextRequired;
        }

        let common_fields = CommonFields {
            schnorr_signature: signature,
            encoded_public_key: PushBytesBuf::from(control_block.internal_key.serialize()),
            block_height: metadata.block_height,
            tx_id: tx.compute_ntxid().into(),
            p2wpkh_address: metadata.signer,
            tx_index: metadata.tx_index,
            output_vout: metadata.output_vout,
        };
        match self.parse_system_message(tx, message_instructions, &common_fields, wallets) {
            Some(message) => CarrierParse::Valid(message),
            None => CarrierParse::Malformed("recognized inscription payload is malformed".into()),
        }
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

        let protocol_version = ProtocolSemanticVersion::try_from_packed(parse_u256(
            instructions.get(3)?.push_bytes()?.as_bytes(),
        )?)
        .ok()?;
        debug!("Parsed protocol version");

        let bootloader_hash = parse_h256(instructions.get(4)?.push_bytes()?.as_bytes())?;
        debug!("Parsed bootloader hash");

        let abstract_account_hash = parse_h256(instructions.get(5)?.push_bytes()?.as_bytes())?;
        debug!("Parsed abstract account hash");

        let snark_wrapper_vk_hash = parse_h256(instructions.get(6)?.push_bytes()?.as_bytes())?;

        let evm_emulator_hash = parse_h256(instructions.get(7)?.push_bytes()?.as_bytes())?;

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

        let l1_batch_hash = parse_h256(instructions.get(2)?.push_bytes()?.as_bytes())?;
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

        let prev_l1_batch_hash = parse_h256(instructions.get(6)?.push_bytes()?.as_bytes())?;
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

        let receiver_l2_address = parse_evm_address(instructions.get(2)?.push_bytes()?.as_bytes())?;
        debug!("Parsed receiver L2 address");

        let l2_contract_address = parse_evm_address(instructions.get(3)?.push_bytes()?.as_bytes())?;
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

        let version = ProtocolSemanticVersion::try_from_packed(parse_u256(
            instructions.get(2)?.push_bytes()?.as_bytes(),
        )?)
        .ok()?;
        debug!("Parsed protocol version");

        let bootloader_code_hash = parse_h256(instructions.get(3)?.push_bytes()?.as_bytes())?;
        debug!("Parsed bootloader code hash");

        let default_account_code_hash = parse_h256(instructions.get(4)?.push_bytes()?.as_bytes())?;
        debug!("Parsed default account code hash");

        let recursion_scheduler_level_vk_hash =
            parse_h256(instructions.get(5)?.push_bytes()?.as_bytes())?;
        debug!("Parsed recursion scheduler level vk hash");

        if !matches!(
            instructions.last(),
            Some(Instruction::Op(opcode)) if *opcode == bitcoin::opcodes::all::OP_ENDIF
        ) {
            return None;
        }

        // The final instruction closes the inscription envelope; only the
        // instructions between the fixed fields and that sentinel are pairs.
        let contract_end = instructions.len().checked_sub(1)?;
        let contract_count = contract_end.checked_sub(6)?;
        if contract_count % 2 != 0 {
            return None;
        }
        let mut system_contracts = Vec::with_capacity(contract_count / 2);

        for i in (6..contract_end).step_by(2) {
            let address = parse_evm_address(instructions.get(i)?.push_bytes()?.as_bytes())?;
            let hash = parse_h256(instructions.get(i + 1)?.push_bytes()?.as_bytes())?;
            system_contracts.push((address, hash))
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
        let mut parser = self.clone();
        for input in &tx.tx.input {
            if input.witness.len() < MIN_WITNESS_LENGTH {
                continue;
            }
            let signer = self.parse_p2wpkh(&input.witness);
            let metadata = CarrierMetadata {
                block_height,
                signer,
                tx_index: Some(tx.tx_index),
                output_vout: tx.output_vout,
            };
            return match parser.parse_system_carrier(input, &tx.tx, metadata, Some(wallets)) {
                CarrierParse::Valid(message)
                    if matches!(message, FullInscriptionMessage::L1ToL2Message(_)) =>
                {
                    Some(message)
                }
                _ => None,
            };
        }
        None
    }

    fn parse_op_return_deposit(
        &self,
        tx: &TransactionWithMetadata,
        block_height: u32,
        bridge_output: &TxOut,
    ) -> Option<FullInscriptionMessage> {
        let output = tx
            .tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;
        let payload = op_return_payload(output).ok()?;
        let signer = tx
            .tx
            .input
            .first()
            .and_then(|input| self.parse_p2wpkh(&input.witness));
        let metadata = CarrierMetadata {
            block_height,
            signer,
            tx_index: Some(tx.tx_index),
            output_vout: tx.output_vout,
        };
        self.parse_op_return_deposit_payload(&tx.tx, payload, bridge_output, metadata)
    }

    fn parse_op_return_withdrawal(
        &self,
        tx: &Transaction,
        block_height: u32,
        wallets: &SystemWallets,
    ) -> Option<FullInscriptionMessage> {
        let output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;
        let payload = op_return_payload(output).ok()?;
        self.parse_op_return_withdrawal_payload(tx, payload, block_height, wallets, None, None)
    }

    fn parse_op_return_protocol_upgrade(
        &self,
        tx: &Transaction,
        block_height: u32,
    ) -> Option<FullInscriptionMessage> {
        let output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;
        let payload = op_return_payload(output).ok()?;
        self.parse_op_return_protocol_upgrade_payload(tx, payload, block_height, None, None)
    }

    fn parse_op_return_update_bridge(
        &self,
        tx: &Transaction,
        block_height: u32,
    ) -> Option<FullInscriptionMessage> {
        let output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;
        let payload = op_return_payload(output).ok()?;
        self.parse_op_return_update_bridge_payload(tx, payload, block_height, None, None)
    }

    fn parse_op_return_update_sequencer(
        &self,
        tx: &Transaction,
        block_height: u32,
    ) -> Option<FullInscriptionMessage> {
        let output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;
        let payload = op_return_payload(output).ok()?;
        self.parse_op_return_update_sequencer_payload(tx, payload, block_height, None, None)
    }

    fn parse_op_return_update_governance(
        &self,
        tx: &Transaction,
        block_height: u32,
    ) -> Option<FullInscriptionMessage> {
        let output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;
        let payload = op_return_payload(output).ok()?;
        self.parse_op_return_update_governance_payload(tx, payload, block_height, None, None)
    }

    pub(super) fn parse_output_carrier(
        &self,
        tx: &Transaction,
        output_index: usize,
        tx_index: usize,
        block_height: u32,
        wallets: Option<&SystemWallets>,
        signer: Option<Address>,
    ) -> CarrierParse<FullInscriptionMessage> {
        let Some(output) = tx.output.get(output_index) else {
            return CarrierParse::Malformed("output index is outside transaction".into());
        };
        if !output.script_pubkey.is_op_return() {
            return CarrierParse::Irrelevant;
        }
        let payload = match op_return_payload(output) {
            Ok(payload) => payload,
            Err(detail) => return CarrierParse::Malformed(detail),
        };

        let bridge_output = wallets.and_then(|wallets| {
            tx.output
                .iter()
                .enumerate()
                .find(|(_, candidate)| candidate.script_pubkey == wallets.bridge.script_pubkey())
        });
        let parsed = if payload.starts_with(OP_RETURN_WITHDRAW_PREFIX) {
            let Some(wallets) = wallets else {
                return CarrierParse::ContextRequired;
            };
            self.parse_op_return_withdrawal_payload(
                tx,
                payload,
                block_height,
                wallets,
                signer,
                Some(tx_index),
            )
        } else if payload.starts_with(OP_RETURN_UPGRADE_PROTOCOL_PREFIX) {
            self.parse_op_return_protocol_upgrade_payload(
                tx,
                payload,
                block_height,
                signer,
                Some(tx_index),
            )
        } else if payload.starts_with(OP_RETURN_UPDATE_BRIDGE_PREFIX) {
            self.parse_op_return_update_bridge_payload(
                tx,
                payload,
                block_height,
                signer,
                Some(tx_index),
            )
        } else if payload.starts_with(OP_RETURN_UPDATE_SEQUENCER_PREFIX) {
            self.parse_op_return_update_sequencer_payload(
                tx,
                payload,
                block_height,
                signer,
                Some(tx_index),
            )
        } else if payload.starts_with(OP_RETURN_UPDATE_GOVERNANCE_PREFIX) {
            self.parse_op_return_update_governance_payload(
                tx,
                payload,
                block_height,
                signer,
                Some(tx_index),
            )
        } else if let Some((bridge_vout, bridge_output)) = bridge_output {
            let metadata = CarrierMetadata {
                block_height,
                signer,
                tx_index: Some(tx_index),
                output_vout: Some(bridge_vout),
            };
            self.parse_op_return_deposit_payload(tx, payload, bridge_output, metadata)
        } else if payload.starts_with(b"VIA_") || payload.starts_with(b"VIA_PROTOCOL:") {
            return CarrierParse::Unsupported(payload.to_vec());
        } else {
            return CarrierParse::Irrelevant;
        };

        match parsed {
            Some(message) => CarrierParse::Valid(message),
            None => CarrierParse::Malformed("recognized OP_RETURN payload is malformed".into()),
        }
    }

    fn parse_op_return_deposit_payload(
        &self,
        tx: &Transaction,
        payload: &[u8],
        bridge_output: &TxOut,
        metadata: CarrierMetadata,
    ) -> Option<FullInscriptionMessage> {
        if payload.starts_with(OP_RETURN_WITHDRAW_PREFIX)
            || payload.starts_with(OP_RETURN_UPGRADE_PROTOCOL_PREFIX)
            || payload.starts_with(OP_RETURN_UPDATE_SEQUENCER_PREFIX)
            || payload.starts_with(OP_RETURN_UPDATE_BRIDGE_PREFIX)
            || payload.starts_with(OP_RETURN_UPDATE_GOVERNANCE_PREFIX)
        {
            return None;
        }
        let receiver_l2_address = parse_evm_address(payload.get(..20)?)?;
        let common = op_return_common_fields(
            tx,
            metadata.block_height,
            metadata.signer,
            metadata.tx_index,
            metadata.output_vout,
        )?;
        Some(FullInscriptionMessage::L1ToL2Message(L1ToL2Message {
            common,
            amount: bridge_output.value,
            input: L1ToL2MessageInput {
                receiver_l2_address,
                l2_contract_address: EVMAddress::zero(),
                call_data: vec![],
            },
            tx_outputs: tx.output.clone(),
        }))
    }

    fn parse_op_return_withdrawal_payload(
        &self,
        tx: &Transaction,
        payload: &[u8],
        block_height: u32,
        wallets: &SystemWallets,
        signer: Option<Address>,
        tx_index: Option<usize>,
    ) -> Option<FullInscriptionMessage> {
        if !payload.starts_with(OP_RETURN_WITHDRAW_PREFIX) {
            return None;
        }
        let version_start = OP_RETURN_WITHDRAW_PREFIX.len();
        let version = WithdrawalVersion::try_from(*payload.get(version_start)?).ok()?;
        let withdrawals_meta = parse_withdrawals(
            version.clone(),
            payload.get(version_start + 1..).unwrap_or_default(),
        )
        .ok()?;
        let mut withdrawals = Vec::new();
        for (index, output) in tx.output.iter().enumerate() {
            let Ok(receiver) = Address::from_script(&output.script_pubkey, self.network) else {
                continue;
            };
            if receiver == wallets.bridge || output.value == Amount::ZERO {
                continue;
            }
            withdrawals.push(L1Withdrawal {
                l2_meta: withdrawals_meta.get(index)?.clone(),
                receiver,
                value: output.value,
            });
        }
        Some(FullInscriptionMessage::BridgeWithdrawal(BridgeWithdrawal {
            common: op_return_common_fields(tx, block_height, signer, tx_index, None)?,
            input: BridgeWithdrawalInput {
                version,
                v_size: tx.vsize() as i64,
                total_size: tx.total_size() as i64,
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                output_amount: tx.output.iter().try_fold(0_u64, |total, output| {
                    total.checked_add(output.value.to_sat())
                })?,
                withdrawals,
            },
        }))
    }

    fn parse_op_return_protocol_upgrade_payload(
        &self,
        tx: &Transaction,
        payload: &[u8],
        block_height: u32,
        signer: Option<Address>,
        tx_index: Option<usize>,
    ) -> Option<FullInscriptionMessage> {
        let proposal_tx_id = parse_prefixed_txid(payload, OP_RETURN_UPGRADE_PROTOCOL_PREFIX)?;
        Some(FullInscriptionMessage::SystemContractUpgrade(
            SystemContractUpgrade {
                common: op_return_common_fields(tx, block_height, signer, tx_index, None)?,
                input: SystemContractUpgradeInput {
                    inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                    proposal_tx_id,
                },
            },
        ))
    }

    fn parse_op_return_update_bridge_payload(
        &self,
        tx: &Transaction,
        payload: &[u8],
        block_height: u32,
        signer: Option<Address>,
        tx_index: Option<usize>,
    ) -> Option<FullInscriptionMessage> {
        let proposal_tx_id = parse_prefixed_txid(payload, OP_RETURN_UPDATE_BRIDGE_PREFIX)?;
        Some(FullInscriptionMessage::UpdateBridge(UpdateBridge {
            common: op_return_common_fields(tx, block_height, signer, tx_index, None)?,
            input: UpdateBridgeInput {
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                proposal_tx_id,
            },
        }))
    }

    fn parse_op_return_update_sequencer_payload(
        &self,
        tx: &Transaction,
        payload: &[u8],
        block_height: u32,
        signer: Option<Address>,
        tx_index: Option<usize>,
    ) -> Option<FullInscriptionMessage> {
        let address = parse_prefixed_address(payload, OP_RETURN_UPDATE_SEQUENCER_PREFIX)?;
        Some(FullInscriptionMessage::UpdateSequencer(UpdateSequencer {
            common: op_return_common_fields(tx, block_height, signer, tx_index, None)?,
            input: UpdateSequencerInput {
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                address,
            },
        }))
    }

    fn parse_op_return_update_governance_payload(
        &self,
        tx: &Transaction,
        payload: &[u8],
        block_height: u32,
        signer: Option<Address>,
        tx_index: Option<usize>,
    ) -> Option<FullInscriptionMessage> {
        let address = parse_prefixed_address(payload, OP_RETURN_UPDATE_GOVERNANCE_PREFIX)?;
        Some(FullInscriptionMessage::UpdateGovernance(UpdateGovernance {
            common: op_return_common_fields(tx, block_height, signer, tx_index, None)?,
            input: UpdateGovernanceInput {
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                address,
            },
        }))
    }
}

fn is_known_system_message_type(message_type: &[u8]) -> bool {
    message_type == types::SYSTEM_BOOTSTRAPPING_MSG.as_bytes()
        || message_type == types::VALIDATOR_ATTESTATION_MSG.as_bytes()
        || message_type == types::L1_BATCH_DA_REFERENCE_MSG.as_bytes()
        || message_type == types::PROOF_DA_REFERENCE_MSG.as_bytes()
        || message_type == types::L1_TO_L2_MSG.as_bytes()
        || message_type == types::SYSTEM_CONTRACT_UPGRADE_MSG.as_bytes()
        || message_type == types::UPGRADE_BRIDGE_MSG.as_bytes()
}

fn parse_h256(bytes: &[u8]) -> Option<H256> {
    (bytes.len() == 32).then(|| H256::from_slice(bytes))
}

fn parse_evm_address(bytes: &[u8]) -> Option<EVMAddress> {
    (bytes.len() == 20).then(|| EVMAddress::from_slice(bytes))
}

fn parse_u256(bytes: &[u8]) -> Option<U256> {
    (bytes.len() <= 32).then(|| U256::from_big_endian(bytes))
}

fn op_return_payload(output: &TxOut) -> Result<&[u8], String> {
    let mut instructions = output.script_pubkey.instructions();
    match instructions.next() {
        Some(Ok(Instruction::Op(op))) if op == bitcoin::opcodes::all::OP_RETURN => {}
        Some(Ok(_)) => return Err("output is not an OP_RETURN carrier".into()),
        Some(Err(err)) => return Err(format!("invalid OP_RETURN script: {err}")),
        None => return Err("empty OP_RETURN script".into()),
    }
    match instructions.next() {
        Some(Ok(Instruction::PushBytes(bytes))) => Ok(bytes.as_bytes()),
        Some(Ok(Instruction::Op(_))) => Err("OP_RETURN payload is not push data".into()),
        Some(Err(err)) => Err(format!("invalid OP_RETURN payload: {err}")),
        None => Err("missing OP_RETURN payload".into()),
    }
}

fn op_return_common_fields(
    tx: &Transaction,
    block_height: u32,
    signer: Option<Address>,
    tx_index: Option<usize>,
    output_vout: Option<usize>,
) -> Option<CommonFields> {
    Some(CommonFields {
        schnorr_signature: TaprootSignature::from_slice(&[0; 64]).ok()?,
        encoded_public_key: PushBytesBuf::new(),
        block_height,
        tx_id: tx.compute_ntxid().into(),
        p2wpkh_address: signer,
        tx_index,
        output_vout,
    })
}

fn parse_prefixed_txid(payload: &[u8], prefix: &[u8]) -> Option<Txid> {
    if !payload.starts_with(prefix) {
        return None;
    }
    let start = prefix.len().checked_add(1)?;
    Txid::from_slice(payload.get(start..start.checked_add(32)?)?).ok()
}

fn parse_prefixed_address(payload: &[u8], prefix: &[u8]) -> Option<Address<NetworkUnchecked>> {
    if !payload.starts_with(prefix) {
        return None;
    }
    let start = prefix.len().checked_add(1)?;
    std::str::from_utf8(payload.get(start..)?)
        .ok()?
        .parse::<Address<NetworkUnchecked>>()
        .ok()
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
    use std::str::FromStr;

    use bitcoin::{consensus::encode::deserialize, hashes::hex::FromHex};

    use super::*;

    fn setup_test_transaction() -> Transaction {
        // TODO: Replace with a real transaction
        let tx_hex = "00001a1abbf8";
        deserialize(&Vec::from_hex(tx_hex).unwrap()).unwrap()
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

    #[ignore]
    #[test]
    fn test_parse_transaction() {
        let network = Network::Bitcoin;
        let mut parser = MessageParser::new(network);
        let tx = setup_test_transaction();

        let messages = parser.parse_system_transaction(&tx, 0, Some(&system_wallets()));
        assert_eq!(messages.len(), 1);
    }

    #[ignore]
    #[test]
    fn test_parse_system_bootstrapping() {
        let network = Network::Bitcoin;
        let mut parser = MessageParser::new(network);
        let tx = setup_test_transaction();

        if let Some(FullInscriptionMessage::SystemBootstrapping(bootstrapping)) = parser
            .parse_system_transaction(&tx, 0, Some(&system_wallets()))
            .pop()
        {
            assert_eq!(bootstrapping.input.start_block_height, 10);
            assert_eq!(bootstrapping.input.verifier_p2wpkh_addresses.len(), 1);
        } else {
            panic!("Expected SystemBootstrapping message");
        }
    }
}
