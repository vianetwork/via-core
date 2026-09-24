use std::str::FromStr;

use bitcoin::{
    address::NetworkUnchecked,
    hashes::Hash,
    script::{Instruction, PushBytesBuf},
    taproot::{ControlBlock, Signature as TaprootSignature},
    Address, Amount, CompressedPublicKey, Network, ScriptBuf, Transaction, TxOut, Txid, Witness,
};
use tracing::{debug, instrument, warn};
use zksync_basic_types::{parse_h160, H256};
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
        if let Some(withdrawal) = self.parse_op_return_withdrawal(&tx.tx, block_height, wallets) {
            messages.push(withdrawal);
        }

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

        if let Some(op_return_message) =
            self.parse_op_return_deposit(tx, block_height, bridge_output)
        {
            messages.push(op_return_message);
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
            block_hash: None,
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

        let protocol_version = ProtocolSemanticVersion::try_from_packed(U256::from_big_endian(
            instructions.get(3)?.push_bytes()?.as_bytes(),
        ))
        .ok()?;
        debug!("Parsed protocol version");

        let bootloader_hash = H256::from_slice(instructions.get(4)?.push_bytes()?.as_bytes());
        debug!("Parsed bootloader hash");

        let abstract_account_hash = H256::from_slice(instructions.get(5)?.push_bytes()?.as_bytes());
        debug!("Parsed abstract account hash");

        let snark_wrapper_vk_hash = H256::from_slice(instructions.get(6)?.push_bytes()?.as_bytes());

        let evm_emulator_hash = H256::from_slice(instructions.get(7)?.push_bytes()?.as_bytes());

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

        let l1_batch_hash = H256::from_slice(instructions.get(2)?.push_bytes()?.as_bytes());
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

        let prev_l1_batch_hash = H256::from_slice(instructions.get(6)?.push_bytes()?.as_bytes());
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

        let receiver_l2_address = parse_h160(instructions.get(2)?.push_bytes()?.as_bytes()).ok()?;
        debug!("Parsed receiver L2 address");

        let l2_contract_address = parse_h160(instructions.get(3)?.push_bytes()?.as_bytes()).ok()?;
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

        let version = ProtocolSemanticVersion::try_from_packed(U256::from_big_endian(
            instructions.get(2)?.push_bytes()?.as_bytes(),
        ))
        .ok()?;
        debug!("Parsed protocol version");

        let bootloader_code_hash = H256::from_slice(instructions.get(3)?.push_bytes()?.as_bytes());
        debug!("Parsed bootloader code hash");

        let default_account_code_hash =
            H256::from_slice(instructions.get(4)?.push_bytes()?.as_bytes());
        debug!("Parsed default account code hash");

        let recursion_scheduler_level_vk_hash =
            H256::from_slice(instructions.get(5)?.push_bytes()?.as_bytes());
        debug!("Parsed recursion scheduler level vk hash");

        let len = instructions.len() - 7;
        let mut system_contracts = Vec::with_capacity(len / 2);

        for i in (6..len).step_by(2) {
            let address = EVMAddress::from_slice(instructions.get(i)?.push_bytes()?.as_bytes());
            let hash = H256::from_slice(instructions.get(i + 1)?.push_bytes()?.as_bytes());
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
        // Try to find any witness data that contains a valid inscription
        for input in tx.tx.input.iter() {
            let witness = &input.witness;
            if witness.len() < MIN_WITNESS_LENGTH {
                continue;
            }

            // Parse signature and control block
            let signature = TaprootSignature::from_slice(&witness[0]).ok()?;
            let script = ScriptBuf::from_bytes(witness[1].to_vec());
            let control_block = ControlBlock::decode(&witness[2]).ok()?;

            let instructions: Vec<_> = script.instructions().filter_map(Result::ok).collect();
            let via_index = find_via_inscription_protocol(&instructions)?;

            // Try to parse p2wpkh address if possible, but make it optional
            let p2wpkh_address = self.parse_p2wpkh(witness);

            let common_fields = CommonFields {
                schnorr_signature: signature,
                encoded_public_key: PushBytesBuf::from(control_block.internal_key.serialize()),
                block_height,
                block_hash: None,
                tx_id: tx.tx.compute_ntxid().into(),
                p2wpkh_address,
                tx_index: Some(tx.tx_index),
                output_vout: tx.output_vout,
            };

            // Parse L1ToL2Message from instructions
            return self.parse_l1_to_l2_message(
                &tx.tx,
                &instructions[via_index..],
                &common_fields,
                Some(wallets),
            );
        }

        None
    }

    fn parse_op_return_deposit(
        &self,
        tx: &TransactionWithMetadata,
        block_height: u32,
        bridge_output: &TxOut,
    ) -> Option<FullInscriptionMessage> {
        // Find OP_RETURN output
        let op_return_output = tx
            .tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;

        let instruction = op_return_output.script_pubkey.instructions().nth(1)?.ok()?;
        if let Some(op_return_data) = instruction.push_bytes() {
            let op_return_data = op_return_data.as_bytes();
            if op_return_data.starts_with(OP_RETURN_WITHDRAW_PREFIX)
                || op_return_data.starts_with(OP_RETURN_UPGRADE_PROTOCOL_PREFIX)
                || op_return_data.starts_with(OP_RETURN_UPDATE_SEQUENCER_PREFIX)
                || op_return_data.starts_with(OP_RETURN_UPDATE_BRIDGE_PREFIX)
                || op_return_data.starts_with(OP_RETURN_UPDATE_GOVERNANCE_PREFIX)
            {
                return None;
            }
            // Parse receiver address from OP_RETURN data

            let receiver_l2_address = EVMAddress::from_slice(op_return_data.get(..20)?);

            let input = L1ToL2MessageInput {
                receiver_l2_address,
                l2_contract_address: EVMAddress::zero(),
                call_data: vec![],
            };

            // Try to parse p2wpkh address from the first input if possible
            let p2wpkh_address = tx
                .tx
                .input
                .first()
                .and_then(|input| self.parse_p2wpkh(&input.witness));

            // Create common fields with empty signature for OP_RETURN
            let common_fields = CommonFields {
                schnorr_signature: TaprootSignature::from_slice(&[0; 64]).ok()?,
                encoded_public_key: PushBytesBuf::new(),
                block_height,
                block_hash: None,
                tx_id: tx.tx.compute_ntxid().into(),
                p2wpkh_address,
                tx_index: Some(tx.tx_index),
                output_vout: tx.output_vout,
            };

            return Some(FullInscriptionMessage::L1ToL2Message(L1ToL2Message {
                common: common_fields,
                amount: bridge_output.value,
                input,
                tx_outputs: tx.tx.output.clone(),
            }));
        }
        None
    }

    pub(super) fn is_withdrawal_candidate(tx: &Transaction) -> bool {
        tx.output.iter().any(|output| {
            let script = output.script_pubkey.as_bytes();
            if !output.script_pubkey.is_op_return() {
                return false;
            }
            // Recognize the protocol even if the declared push length is truncated.
            // A credible wallet spend must then defer, not disappear as unrelated data.
            let offset = match script.get(1) {
                Some(0x01..=0x4b) => 2,
                Some(0x4c) => 3,
                Some(0x4d) => 4,
                Some(0x4e) => 6,
                _ => return false,
            };
            script
                .get(offset..)
                .is_some_and(|data| data.starts_with(OP_RETURN_WITHDRAW_PREFIX))
        })
    }

    pub(super) fn parse_op_return_withdrawal(
        &self,
        tx: &Transaction,
        block_height: u32,
        wallets: &SystemWallets,
    ) -> Option<FullInscriptionMessage> {
        // Find OP_RETURN output
        let op_return_output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;

        let mut instructions = op_return_output.script_pubkey.instructions();
        instructions.next()?.ok()?;
        let data = instructions.next()?.ok()?;
        if instructions.next().is_some() {
            return None;
        }
        if let Some(op_return_data) = data.push_bytes().map(|bytes| bytes.as_bytes()) {
            if !op_return_data.starts_with(OP_RETURN_WITHDRAW_PREFIX) {
                return None;
            }

            let version_start = OP_RETURN_WITHDRAW_PREFIX.len();
            let version = *op_return_data.get(version_start)?;

            let version = match WithdrawalVersion::try_from(version) {
                Ok(version) => version,
                Err(_) => {
                    tracing::warn!("Failed to parse the withdrawal version");
                    return None;
                }
            };

            let msg_start = version_start + 1;

            let withdrawals_meta =
                match parse_withdrawals(version.clone(), &op_return_data[msg_start..]) {
                    Ok(result) => result,
                    Err(err) => {
                        tracing::warn!(
                            "Failed to parse the withdrawals, version: {:?}, error: {}",
                            version,
                            err
                        );
                        return None;
                    }
                };

            // Metadata positions identify payment outputs, including zero-net and bridge
            // recipients. Only the optional trailing change belongs to the wallet.
            let count = withdrawals_meta.len();
            if count == 0 || !(tx.output.len() == count + 1 || tx.output.len() == count + 2) {
                return None;
            }
            if !tx.output.get(count)?.script_pubkey.is_op_return() {
                return None;
            }
            if let Some(change) = tx.output.get(count + 1) {
                if change.script_pubkey != wallets.bridge.script_pubkey() {
                    return None;
                }
            }
            let mut withdrawals = Vec::with_capacity(count);
            for (i, meta) in withdrawals_meta.into_iter().enumerate() {
                let output = &tx.output[i];
                withdrawals.push(L1Withdrawal {
                    vout: u32::try_from(i).ok()?,
                    l2_meta: meta,
                    receiver: Address::from_script(&output.script_pubkey, self.network).ok()?,
                    value: output.value,
                });
            }

            let input = BridgeWithdrawalInput {
                transaction: tx.clone(),
                prevouts: Vec::new(),
                block_hash: None,
                bridge_script_pubkey: None,
                version,
                v_size: tx.vsize() as i64,
                total_size: tx.total_size() as i64,
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                output_amount: tx.output.iter().map(|out| out.value.to_sat()).sum(),
                withdrawals,
            };

            // Create common fields with empty signature for OP_RETURN
            let common_fields = CommonFields {
                schnorr_signature: TaprootSignature::from_slice(&[0; 64]).ok()?,
                encoded_public_key: PushBytesBuf::new(),
                block_height,
                block_hash: None,
                tx_id: tx.compute_txid(),
                p2wpkh_address: None,
                tx_index: None,
                output_vout: None,
            };

            return Some(FullInscriptionMessage::BridgeWithdrawal(BridgeWithdrawal {
                common: common_fields,
                input,
            }));
        }
        None
    }

    fn parse_op_return_protocol_upgrade(
        &self,
        tx: &Transaction,
        block_height: u32,
    ) -> Option<FullInscriptionMessage> {
        // Find OP_RETURN output
        let op_return_output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;

        // Parse OP_RETURN data
        if let Some(op_return_data) = op_return_output.script_pubkey.as_bytes().get(2..) {
            if !op_return_data.starts_with(OP_RETURN_UPGRADE_PROTOCOL_PREFIX) {
                return None;
            }

            let start = OP_RETURN_UPGRADE_PROTOCOL_PREFIX.len() + 1;
            if op_return_data.len() < start + 32 {
                return None;
            }

            // Parse proposal_tx_id from OP_RETURN data
            let proposal_tx_id = match Txid::from_slice(&op_return_data[start..start + 32]) {
                Ok(tx_id) => tx_id,
                Err(_) => return None,
            };

            let input = SystemContractUpgradeInput {
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                proposal_tx_id,
            };

            // Create common fields with empty signature for OP_RETURN
            let common_fields = CommonFields {
                schnorr_signature: TaprootSignature::from_slice(&[0; 64]).ok()?,
                encoded_public_key: PushBytesBuf::new(),
                block_height,
                block_hash: None,
                tx_id: tx.compute_ntxid().into(),
                p2wpkh_address: None,
                tx_index: None,
                output_vout: None,
            };

            return Some(FullInscriptionMessage::SystemContractUpgrade(
                SystemContractUpgrade {
                    common: common_fields,
                    input,
                },
            ));
        }
        None
    }

    fn parse_op_return_update_bridge(
        &self,
        tx: &Transaction,
        block_height: u32,
    ) -> Option<FullInscriptionMessage> {
        // Find OP_RETURN output
        let op_return_output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;

        // Parse OP_RETURN data
        if let Some(op_return_data) = op_return_output.script_pubkey.as_bytes().get(2..) {
            if !op_return_data.starts_with(OP_RETURN_UPDATE_BRIDGE_PREFIX) {
                return None;
            }

            let start = OP_RETURN_UPDATE_BRIDGE_PREFIX.len() + 1;
            if op_return_data.len() < start + 32 {
                return None;
            }

            // Parse proposal_tx_id from OP_RETURN data
            let proposal_tx_id = match Txid::from_slice(&op_return_data[start..start + 32]) {
                Ok(tx_id) => tx_id,
                Err(_) => return None,
            };

            let input = UpdateBridgeInput {
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                proposal_tx_id,
            };

            // Create common fields with empty signature for OP_RETURN
            let common = CommonFields {
                schnorr_signature: TaprootSignature::from_slice(&[0; 64]).ok()?,
                encoded_public_key: PushBytesBuf::new(),
                block_height,
                block_hash: None,
                tx_id: tx.compute_ntxid().into(),
                p2wpkh_address: None,
                tx_index: None,
                output_vout: None,
            };

            return Some(FullInscriptionMessage::UpdateBridge(UpdateBridge {
                common,
                input,
            }));
        }
        None
    }

    fn parse_op_return_update_sequencer(
        &self,
        tx: &Transaction,
        block_height: u32,
    ) -> Option<FullInscriptionMessage> {
        // Find OP_RETURN output
        let op_return_output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;

        // Parse OP_RETURN data
        if let Some(op_return_data) = op_return_output.script_pubkey.as_bytes().get(2..) {
            if !op_return_data.starts_with(OP_RETURN_UPDATE_SEQUENCER_PREFIX) {
                return None;
            }

            let start = OP_RETURN_UPDATE_SEQUENCER_PREFIX.len() + 1;

            // Parse sequencer address from OP_RETURN data
            let address_str = std::str::from_utf8(&op_return_data[start..]).ok()?;
            let address = match Address::from_str(address_str) {
                Ok(address) => address,
                Err(_) => return None,
            };

            let input = UpdateSequencerInput {
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                address,
            };

            // Create common fields with empty signature for OP_RETURN
            let common = CommonFields {
                schnorr_signature: TaprootSignature::from_slice(&[0; 64]).ok()?,
                encoded_public_key: PushBytesBuf::new(),
                block_height,
                block_hash: None,
                tx_id: tx.compute_ntxid().into(),
                p2wpkh_address: None,
                tx_index: None,
                output_vout: None,
            };

            return Some(FullInscriptionMessage::UpdateSequencer(UpdateSequencer {
                common,
                input,
            }));
        }
        None
    }

    fn parse_op_return_update_governance(
        &self,
        tx: &Transaction,
        block_height: u32,
    ) -> Option<FullInscriptionMessage> {
        // Find OP_RETURN output
        let op_return_output = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())?;

        // Parse OP_RETURN data
        if let Some(op_return_data) = op_return_output.script_pubkey.as_bytes().get(2..) {
            if !op_return_data.starts_with(OP_RETURN_UPDATE_GOVERNANCE_PREFIX) {
                return None;
            }

            let start = OP_RETURN_UPDATE_GOVERNANCE_PREFIX.len() + 1;

            // Parse sequencer address from OP_RETURN data
            let address_str = std::str::from_utf8(&op_return_data[start..]).ok()?;
            let address = match Address::from_str(address_str) {
                Ok(address) => address,
                Err(_) => return None,
            };

            let input = UpdateGovernanceInput {
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                address,
            };

            // Create common fields with empty signature for OP_RETURN
            let common = CommonFields {
                schnorr_signature: TaprootSignature::from_slice(&[0; 64]).ok()?,
                encoded_public_key: PushBytesBuf::new(),
                block_height,
                block_hash: None,
                tx_id: tx.compute_ntxid().into(),
                p2wpkh_address: None,
                tx_index: None,
                output_vout: None,
            };

            return Some(FullInscriptionMessage::UpdateGovernance(UpdateGovernance {
                common,
                input,
            }));
        }
        None
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
pub(super) mod tests {
    use bitcoin::{
        absolute,
        consensus::encode::deserialize,
        hashes::hex::FromHex,
        opcodes::{all, OP_FALSE},
        script::Builder,
        secp256k1::{Keypair, Secp256k1, SecretKey},
        taproot::LeafVersion,
        transaction, TxIn,
    };

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

    pub(in crate::indexer) fn inscription_witness(receiver: &[u8], contract: &[u8]) -> Witness {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(&[1; 32]).unwrap();
        let keypair = Keypair::from_secret_key(&secp, &secret_key);
        let (internal_key, _) = keypair.x_only_public_key();
        let script = Builder::new()
            .push_slice(internal_key.serialize())
            .push_opcode(all::OP_CHECKSIG)
            .push_opcode(OP_FALSE)
            .push_opcode(all::OP_IF)
            .push_slice(b"via_inscription_protocol")
            .push_slice(b"L1ToL2Message")
            .push_slice(PushBytesBuf::try_from(receiver.to_vec()).unwrap())
            .push_slice(PushBytesBuf::try_from(contract.to_vec()).unwrap())
            .push_slice(PushBytesBuf::new())
            .push_opcode(all::OP_ENDIF)
            .into_script();
        let mut control_block = vec![LeafVersion::TapScript.to_consensus()];
        control_block.extend(internal_key.serialize());

        Witness::from_slice(&[vec![0; 64], script.into_bytes(), control_block])
    }

    fn bridge_transaction(op_return_body: &[u8], tx_index: usize) -> TransactionWithMetadata {
        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![
                TxOut {
                    value: Amount::from_sat(100_000),
                    script_pubkey: system_wallets().bridge.script_pubkey(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::new_op_return(
                        PushBytesBuf::try_from(op_return_body.to_vec()).unwrap(),
                    ),
                },
            ],
        };
        TransactionWithMetadata::new(tx, tx_index)
    }

    fn assert_deposit(
        messages: &[FullInscriptionMessage],
        receiver: EVMAddress,
        block_height: u32,
        tx_index: usize,
    ) {
        let [FullInscriptionMessage::L1ToL2Message(deposit)] = messages else {
            panic!("expected one L1-to-L2 deposit, got {messages:?}");
        };
        assert_eq!(deposit.input.receiver_l2_address, receiver);
        assert_eq!(deposit.input.l2_contract_address, EVMAddress::zero());
        assert_eq!(deposit.input.call_data, Vec::<u8>::new());
        assert_eq!(deposit.amount, Amount::from_sat(100_000));
        assert_eq!(deposit.common.block_height, block_height);
        assert_eq!(deposit.common.tx_index, Some(tx_index));
        assert_eq!(deposit.common.output_vout, Some(0));
    }

    #[test]
    fn truncated_op_return_deposit_is_rejected_without_stopping_parser() {
        let wallets = system_wallets();
        let mut parser = MessageParser::new(Network::Regtest);

        let mut truncated = bridge_transaction(&[0x55; 19], 7);
        let messages = parser.parse_bridge_transaction(&mut truncated, 42, &wallets);
        assert!(messages.is_empty());

        let mut control = bridge_transaction(&[0x81; 20], 7);
        let messages = parser.parse_bridge_transaction(&mut control, 42, &wallets);
        assert_deposit(&messages, EVMAddress::repeat_byte(0x81), 42, 7);

        let mut with_companion = bridge_transaction(&[0x55; 19], 9);
        with_companion.tx.input[0].witness = inscription_witness(&[0x83; 20], &[0; 20]);
        let messages = parser.parse_bridge_transaction(&mut with_companion, 44, &wallets);
        assert_deposit(&messages, EVMAddress::repeat_byte(0x83), 44, 9);

        let mut extended_body = [0x84; 21];
        extended_body[20] = 0xff;
        let mut extended = bridge_transaction(&extended_body, 10);
        let messages = parser.parse_bridge_transaction(&mut extended, 45, &wallets);
        assert_deposit(&messages, EVMAddress::repeat_byte(0x84), 45, 10);
    }

    #[test]
    fn withdrawal_without_version_preserves_companion_deposit() {
        let wallets = system_wallets();
        let mut parser = MessageParser::new(Network::Regtest);
        let mut tx = bridge_transaction(b"VIA_WI", 7);
        tx.tx.input[0].witness = inscription_witness(&[0x83; 20], &[0; 20]);
        let messages = parser.parse_bridge_transaction(&mut tx, 42, &wallets);
        assert_deposit(&messages, EVMAddress::repeat_byte(0x83), 42, 7);
    }

    #[test]
    fn withdrawal_requires_metadata_at_each_recipient_output_index() {
        let wallets = system_wallets();
        let mut parser = MessageParser::new(Network::Regtest);
        let body = [b"VIA_WI\0".as_slice(), &[0x90; 10]].concat();
        let mut tx = bridge_transaction(&body, 7);
        let recipient = TxOut {
            value: Amount::from_sat(10_000),
            script_pubkey: wallets.sequencer.script_pubkey(),
        };
        tx.tx.output.insert(0, recipient.clone());
        tx.tx.output.swap(1, 2);

        let messages = parser.parse_bridge_transaction(&mut tx, 42, &wallets);
        let [FullInscriptionMessage::BridgeWithdrawal(withdrawal)] = messages.as_slice() else {
            panic!("expected one withdrawal, got {messages:?}");
        };
        assert_eq!(withdrawal.input.withdrawals.len(), 1);
        let recipient_withdrawal = &withdrawal.input.withdrawals[0];
        assert_eq!(recipient_withdrawal.receiver, wallets.sequencer);
        assert_eq!(recipient_withdrawal.value, Amount::from_sat(10_000));
        assert_eq!(recipient_withdrawal.l2_meta.l2_id, "90909090909090909090");
        assert_eq!(recipient_withdrawal.l2_meta.l2_tx_event_index, 0x9090);

        let mut shifted = tx.clone();
        shifted.tx.output.swap(0, 2);
        let mut partial = tx;
        partial.tx.output.insert(1, recipient);
        for mut malformed in [shifted, partial] {
            assert!(parser
                .parse_bridge_transaction(&mut malformed, 42, &wallets)
                .is_empty());
        }
    }

    #[test]
    fn withdrawal_preserves_bridge_zero_net_and_repeated_output_positions_without_change() {
        let wallets = system_wallets();
        let mut parser = MessageParser::new(Network::Regtest);
        let body = [
            b"VIA_WI\0".as_slice(),
            &[0x11; 10],
            &[0x22; 10],
            &[0x33; 10],
        ]
        .concat();
        let mut tx = bridge_transaction(&body, 0);
        let metadata = tx.tx.output.pop().unwrap();
        tx.tx.output = vec![
            TxOut {
                value: Amount::ZERO,
                script_pubkey: wallets.bridge.script_pubkey(),
            },
            TxOut {
                value: Amount::from_sat(800),
                script_pubkey: wallets.sequencer.script_pubkey(),
            },
            TxOut {
                value: Amount::from_sat(900),
                script_pubkey: wallets.sequencer.script_pubkey(),
            },
            metadata,
        ];
        tx.tx.input[0].script_sig = ScriptBuf::from_bytes(vec![0x51]);
        let messages = parser.parse_bridge_transaction(&mut tx, 42, &wallets);
        let [FullInscriptionMessage::BridgeWithdrawal(payment)] = messages.as_slice() else {
            panic!("expected payment, got {messages:?}");
        };
        assert_eq!(payment.common.tx_id, tx.tx.compute_txid());
        assert_eq!(payment.input.transaction, tx.tx);
        let outputs = &payment.input.withdrawals;
        assert_eq!(
            outputs.iter().map(|output| output.vout).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(outputs[0].receiver, wallets.bridge);
        assert_eq!(outputs[0].value, Amount::ZERO);
        assert_eq!(outputs[1].l2_meta.l2_id, "22222222222222222222");
        assert_eq!(outputs[2].l2_meta.l2_id, "33333333333333333333");

        tx.tx.output.remove(0);
        tx.tx.output.pop();
        tx.tx.output.push(TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return(
                PushBytesBuf::try_from([b"VIA_WI\0".as_slice(), &[0x22; 10], &[0x33; 10]].concat())
                    .unwrap(),
            ),
        });
        let messages = parser.parse_bridge_transaction(&mut tx, 42, &wallets);
        assert!(matches!(
            messages.as_slice(),
            [FullInscriptionMessage::BridgeWithdrawal(_)]
        ));
    }

    #[test]
    fn op_return_deposit_preserves_receiver_and_canonical_identity() {
        let wallets = system_wallets();
        let mut parser = MessageParser::new(Network::Regtest);
        for (header, payload_len) in [
            (vec![0x6a, 0x14], 20),
            (vec![0x6a, 0x49], 73),
            (vec![0x6a, 0x4a], 74),
            (vec![0x6a, 0x4b], 75),
            (vec![0x6a, 0x4c, 0x14], 20),
            (vec![0x6a, 0x4c, 0x4c], 76),
            (vec![0x6a, 0x4d, 0x14, 0], 20),
            (vec![0x6a, 0x4e, 0x14, 0, 0, 0], 20),
        ] {
            let script = [header, vec![0x81; 20], vec![0x55; payload_len - 20]].concat();
            let mut tx = bridge_transaction(&[0x81; 20], 7);
            tx.tx.output[1].script_pubkey = ScriptBuf::from_bytes(script);
            let messages = parser.parse_bridge_transaction(&mut tx, 42, &wallets);
            assert_deposit(&messages, EVMAddress::repeat_byte(0x81), 42, 7);
            let FullInscriptionMessage::L1ToL2Message(deposit) = &messages[0] else {
                unreachable!();
            };
            let l1_tx = zksync_types::l1::via_l1::ViaL1Deposit {
                l2_receiver_address: deposit.input.receiver_l2_address,
                amount: deposit.amount.to_sat(),
                calldata: deposit.input.call_data.clone(),
                l1_block_number: deposit.common.block_height.into(),
                tx_index: deposit.common.tx_index.unwrap(),
                output_vout: deposit.common.output_vout.unwrap(),
            }
            .l1_tx()
            .unwrap();
            assert_eq!(
                l1_tx.common_data.canonical_tx_hash,
                H256::from_str("2172c0d2f6e012bd2fcc15922027acf482766adeeced17143459af9d56b8dd04")
                    .unwrap()
            );
            assert_eq!(
                l1_tx.execute.contract_address,
                Some(EVMAddress::repeat_byte(0x81))
            );
            assert_eq!(
                l1_tx.common_data.refund_recipient,
                EVMAddress::repeat_byte(0x81)
            );
        }

        let receiver = hex::decode("8182838485868788898a8b8c8d8e8f9091929394").unwrap();
        let mut tx = bridge_transaction(&receiver, 7);
        let messages = parser.parse_bridge_transaction(&mut tx, 42, &wallets);
        assert_deposit(
            &messages,
            EVMAddress::from_str("8182838485868788898a8b8c8d8e8f9091929394").unwrap(),
            42,
            7,
        );
    }

    #[test]
    fn op_return_deposit_uses_the_first_push_boundary() {
        let wallets = system_wallets();
        let mut parser = MessageParser::new(Network::Regtest);
        for tail in [vec![0x4c], vec![0x75; 80]] {
            let mut tx = bridge_transaction(&[0x81; 20], 7);
            let script = [tx.tx.output[1].script_pubkey.as_bytes(), &tail].concat();
            tx.tx.output[1].script_pubkey = ScriptBuf::from_bytes(script);
            assert_deposit(
                &parser.parse_bridge_transaction(&mut tx, 42, &wallets),
                EVMAddress::repeat_byte(0x81),
                42,
                7,
            );
        }

        for script in [
            [vec![0x6a, 0x13], vec![0x81; 20]].concat(),
            [vec![0x6a, 0x15], vec![0x81; 20]].concat(),
            [vec![0x6a, 0x4e, 0xff, 0xff, 0xff, 0xff], vec![0x81; 20]].concat(),
            [vec![0x6a, 0x75], vec![0x81; 20]].concat(),
            [vec![0x6a, 0x4a], b"VIA_WI".to_vec(), vec![0xff; 68]].concat(),
            [vec![0x6a, 0x4c, 0x14], b"VIA_WI".to_vec(), vec![0xff; 14]].concat(),
        ] {
            let mut tx = bridge_transaction(&[0x81; 20], 7);
            tx.tx.output[1].script_pubkey = ScriptBuf::from_bytes(script.clone());
            let messages = parser.parse_bridge_transaction(&mut tx, 42, &wallets);
            assert!(
                messages.is_empty(),
                "script {}: {messages:?}",
                hex::encode(script)
            );
        }
    }

    #[test]
    fn inscription_addresses_require_exactly_twenty_bytes() {
        let wallets = system_wallets();
        let mut parser = MessageParser::new(Network::Regtest);

        for (receiver_len, contract_len) in
            [(0, 20), (19, 20), (21, 20), (20, 0), (20, 19), (20, 21)]
        {
            let mut companion = bridge_transaction(&[0x81; 20], 8);
            companion.tx.input[0].witness =
                inscription_witness(&vec![0x55; receiver_len], &vec![0x66; contract_len]);
            let messages = parser.parse_bridge_transaction(&mut companion, 42, &wallets);
            assert_deposit(&messages, EVMAddress::repeat_byte(0x81), 42, 8);
        }

        let mut valid = bridge_transaction(&[], 9);
        valid.tx.input[0].witness = inscription_witness(&[0x82; 20], &[0x83; 20]);
        let messages = parser.parse_bridge_transaction(&mut valid, 43, &wallets);
        let [FullInscriptionMessage::L1ToL2Message(deposit)] = messages.as_slice() else {
            panic!("expected one inscription deposit, got {messages:?}");
        };
        assert_eq!(
            deposit.input.receiver_l2_address,
            EVMAddress::repeat_byte(0x82)
        );
        assert_eq!(
            deposit.input.l2_contract_address,
            EVMAddress::repeat_byte(0x83)
        );
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
