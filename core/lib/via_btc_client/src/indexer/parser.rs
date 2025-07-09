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
    protocol_version::ProtocolSemanticVersion, Address as EVMAddress, L1BatchNumber, U256,
};

use crate::types::{
    self, BridgeWithdrawal, BridgeWithdrawalInput, CommonFields, FullInscriptionMessage,
    L1BatchDAReference, L1BatchDAReferenceInput, L1ToL2Message, L1ToL2MessageInput,
    ProofDAReference, ProofDAReferenceInput, ProposeSequencer, ProposeSequencerInput,
    SystemBootstrapping, SystemBootstrappingInput, SystemContractUpgrade,
    SystemContractUpgradeInput, TransactionWithMetadata, ValidatorAttestation,
    ValidatorAttestationInput, Vote,
};

const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL";

// Using constants to define the minimum number of instructions can help to make parsing more quick
const MIN_WITNESS_LENGTH: usize = 3;
const MIN_SYSTEM_BOOTSTRAPPING_INSTRUCTIONS: usize = 8;
const MIN_PROPOSE_SEQUENCER_INSTRUCTIONS: usize = 3;
const MIN_VALIDATOR_ATTESTATION_INSTRUCTIONS: usize = 4;
const MIN_L1_BATCH_DA_REFERENCE_INSTRUCTIONS: usize = 7;
const MIN_PROOF_DA_REFERENCE_INSTRUCTIONS: usize = 5;
const MIN_L1_TO_L2_MESSAGE_INSTRUCTIONS: usize = 5;
const MIN_SYSTEM_CONTRACT_UPGRADE_PROPOSAL: usize = 6;

#[derive(Debug, Clone)]
pub struct MessageParser {
    network: Network,
    bridge_address: Option<Address>,
}

impl MessageParser {
    pub fn new(network: Network) -> Self {
        Self {
            network,
            bridge_address: None,
        }
    }

    #[instrument(skip(self, tx), target = "bitcoin_indexer::parser")]
    pub fn parse_system_transaction(
        &mut self,
        tx: &Transaction,
        block_height: u32,
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
                        self.parse_system_input(input, tx, block_height, address.clone())
                    })
                    .collect()
            }
            None => {
                vec![]
            }
        }
    }

    #[instrument(skip(self, tx), target = "bitcoin_indexer::parser")]
    pub fn parse_bridge_transaction(
        &mut self,
        tx: &mut TransactionWithMetadata,
        block_height: u32,
    ) -> Vec<FullInscriptionMessage> {
        let mut messages = Vec::new();

        let vout = match tx.tx.output.iter().enumerate().find_map(|(index, output)| {
            if let Some(bridge_addr) = &self.bridge_address {
                if output.script_pubkey == bridge_addr.script_pubkey() {
                    Some(index)
                } else {
                    None
                }
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
        if let Some(inscription_message) = self.parse_inscription_deposit(tx, block_height) {
            messages.push(inscription_message);
        }

        // If not an inscription, try to parse as OP_RETURN based deposit
        if let Some(op_return_message) =
            self.parse_op_return_deposit(tx, block_height, bridge_output)
        {
            messages.push(op_return_message);
        }

        // Try to parse withdrawals processed by the bridge address.
        if let Some(bridge_withdrawals) = self.parse_op_return_withdrawal(&tx.tx, block_height) {
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

        self.parse_system_message(tx, &instructions[via_index..], &common_fields)
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
                if bytes.as_bytes() == types::PROPOSE_SEQUENCER_MSG.as_bytes() =>
            {
                debug!("Parsing propose sequencer message");
                self.parse_propose_sequencer(instructions, common_fields)
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
                self.parse_l1_to_l2_message(tx, instructions, common_fields)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::SYSTEM_CONTRACT_UPGRADE_MSG.as_bytes() =>
            {
                debug!("Parsing System contract upgrade proposal");
                self.parse_system_contract_upgrade_message(instructions, common_fields)
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

        // network unchecked is required to enable serde serialization and deserialization on the library structs
        let network_unchecked_verifier_addresses = instructions[3..instructions.len() - 5]
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

        let network_unchecked_bridge_address =
            instructions.get(instructions.len() - 5).and_then(|instr| {
                if let Instruction::PushBytes(bytes) = instr {
                    std::str::from_utf8(bytes.as_bytes())
                        .ok()
                        .and_then(|s| s.parse::<Address<NetworkUnchecked>>().ok())
                } else {
                    None
                }
            })?;

        // Save the bridge address for later use
        self.bridge_address = Some(
            network_unchecked_bridge_address
                .clone()
                .require_network(self.network)
                .ok()?,
        );

        debug!("Parsed bridge address");

        let bootloader_hash = H256::from_slice(
            instructions
                .get(instructions.len() - 4)?
                .push_bytes()?
                .as_bytes(),
        );

        debug!("Parsed bootloader hash");

        let abstract_account_hash = H256::from_slice(
            instructions
                .get(instructions.len() - 3)?
                .push_bytes()?
                .as_bytes(),
        );

        debug!("Parsed abstract account hash");

        let network_unchecked_governance_address =
            instructions.get(instructions.len() - 2).and_then(|instr| {
                if let Instruction::PushBytes(bytes) = instr {
                    std::str::from_utf8(bytes.as_bytes())
                        .ok()
                        .and_then(|s| s.parse::<Address<NetworkUnchecked>>().ok())
                } else {
                    None
                }
            })?;

        debug!("Parsed governance address");

        Some(FullInscriptionMessage::SystemBootstrapping(
            SystemBootstrapping {
                common: common_fields.clone(),
                input: SystemBootstrappingInput {
                    start_block_height,
                    bridge_musig2_address: network_unchecked_bridge_address,
                    verifier_p2wpkh_addresses: network_unchecked_verifier_addresses,
                    bootloader_hash,
                    abstract_account_hash,
                    governance_address: network_unchecked_governance_address,
                },
            },
        ))
    }

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_propose_sequencer(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<FullInscriptionMessage> {
        if instructions.len() < MIN_PROPOSE_SEQUENCER_INSTRUCTIONS {
            warn!("Insufficient instructions for propose sequencer");
            return None;
        }

        let sequencer_address = instructions.get(2).and_then(|instr| {
            if let Instruction::PushBytes(bytes) = instr {
                std::str::from_utf8(bytes.as_bytes()).ok().and_then(|s| {
                    s.parse::<Address<NetworkUnchecked>>()
                        .ok()?
                        .require_network(self.network)
                        .ok()
                })
            } else {
                None
            }
        })?;

        debug!("Parsed sequencer address");

        Some(FullInscriptionMessage::ProposeSequencer(ProposeSequencer {
            common: common_fields.clone(),
            input: ProposeSequencerInput {
                sequencer_new_p2wpkh_address: sequencer_address.as_unchecked().clone(),
            },
        }))
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
    ) -> Option<FullInscriptionMessage> {
        if instructions.len() < MIN_L1_TO_L2_MESSAGE_INSTRUCTIONS {
            warn!("Insufficient instructions for L1 to L2 message");
            return None;
        }

        let receiver_l2_address =
            EVMAddress::from_slice(instructions.get(2)?.push_bytes()?.as_bytes());
        debug!("Parsed receiver L2 address");

        let l2_contract_address =
            EVMAddress::from_slice(instructions.get(3)?.push_bytes()?.as_bytes());
        debug!("Parsed L2 contract address");

        let call_data = instructions.get(4)?.push_bytes()?.as_bytes().to_vec();
        debug!("Parsed call data, length: {}", call_data.len());

        let amount = tx
            .output
            .iter()
            .find(|output| {
                if let Some(address) = self.bridge_address.as_ref() {
                    output.script_pubkey.is_p2tr()
                        && output.script_pubkey == address.script_pubkey()
                } else {
                    tracing::error!("Bridge address not found");
                    false
                }
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

        Some(FullInscriptionMessage::SystemContractUpgrade(
            SystemContractUpgrade {
                common: common_fields.clone(),
                input: SystemContractUpgradeInput {
                    version,
                    bootloader_code_hash,
                    default_account_code_hash,
                    recursion_scheduler_level_vk_hash,
                    system_contracts,
                },
            },
        ))
    }

    fn parse_inscription_deposit(
        &self,
        tx: &TransactionWithMetadata,
        block_height: u32,
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
                tx_id: tx.tx.compute_ntxid().into(),
                p2wpkh_address,
                tx_index: Some(tx.tx_index),
                output_vout: tx.output_vout,
            };

            // Parse L1ToL2Message from instructions
            return self.parse_l1_to_l2_message(&tx.tx, &instructions[via_index..], &common_fields);
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

        // Parse OP_RETURN data
        let op_return_data = op_return_output.script_pubkey.as_bytes();
        if op_return_data.len() < 2 {
            return None;
        }

        // Parse OP_RETURN data
        if let Some(op_return_data) = op_return_output.script_pubkey.as_bytes().get(2..) {
            if op_return_data.starts_with(OP_RETURN_WITHDRAW_PREFIX) {
                return None;
            }
            // Parse receiver address from OP_RETURN data

            let receiver_l2_address = EVMAddress::from_slice(&op_return_data[0..20]);

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

    fn parse_op_return_withdrawal(
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
            if !op_return_data.starts_with(OP_RETURN_WITHDRAW_PREFIX) {
                return None;
            }

            let start = OP_RETURN_WITHDRAW_PREFIX.len() + 1;
            // Parse l1_batch_reveal_tx_id from OP_RETURN data
            let l1_batch_proof_reveal_tx_id =
                match Txid::from_slice(&op_return_data[start..start + 32]) {
                    Ok(tx_id) => tx_id.as_raw_hash().as_byte_array().to_vec(),
                    Err(_) => return None,
                };

            let mut withdrawals = Vec::new();
            for output in &tx.output {
                let address = match Address::from_script(&output.script_pubkey, self.network) {
                    Ok(address) => address,
                    Err(_) => continue,
                };

                if let Some(bridge_address) = self.bridge_address.clone() {
                    if address == bridge_address {
                        continue;
                    }
                } else {
                    return None;
                }

                withdrawals.push((address.to_string(), output.value.to_sat() as i64));
            }

            let input = BridgeWithdrawalInput {
                v_size: tx.vsize() as i64,
                total_size: tx.total_size() as i64,
                inputs: tx.input.iter().map(|input| input.previous_output).collect(),
                output_amount: tx.output.iter().map(|out| out.value.to_sat()).sum(),
                l1_batch_proof_reveal_tx_id,
                withdrawals,
            };

            // Create common fields with empty signature for OP_RETURN
            let common_fields = CommonFields {
                schnorr_signature: TaprootSignature::from_slice(&[0; 64]).ok()?,
                encoded_public_key: PushBytesBuf::new(),
                block_height,
                tx_id: tx.compute_ntxid().into(),
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
    use bitcoin::{consensus::encode::deserialize, hashes::hex::FromHex};

    use super::*;

    fn setup_test_transaction() -> Transaction {
        // TODO: Replace with a real transaction
        let tx_hex = "00001a1abbf8";
        deserialize(&Vec::from_hex(tx_hex).unwrap()).unwrap()
    }

    #[ignore]
    #[test]
    fn test_parse_transaction() {
        let network = Network::Bitcoin;
        let mut parser = MessageParser::new(network);
        let tx = setup_test_transaction();

        let messages = parser.parse_system_transaction(&tx, 0);
        assert_eq!(messages.len(), 1);
    }

    #[ignore]
    #[test]
    fn test_parse_system_bootstrapping() {
        let network = Network::Bitcoin;
        let mut parser = MessageParser::new(network);
        let tx = setup_test_transaction();

        if let Some(FullInscriptionMessage::SystemBootstrapping(bootstrapping)) =
            parser.parse_system_transaction(&tx, 0).pop()
        {
            assert_eq!(bootstrapping.input.start_block_height, 10);
            assert_eq!(bootstrapping.input.verifier_p2wpkh_addresses.len(), 1);
        } else {
            panic!("Expected SystemBootstrapping message");
        }
    }
}
