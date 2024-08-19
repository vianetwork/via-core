use crate::types;
use crate::types::{
    CommonFields, FullInscriptionMessage as Message, L1BatchDAReference, L1BatchDAReferenceInput,
    L1ToL2Message, L1ToL2MessageInput, ProofDAReference, ProofDAReferenceInput, ProposeSequencer,
    ProposeSequencerInput, SystemBootstrapping, SystemBootstrappingInput, ValidatorAttestation,
    ValidatorAttestationInput, Vote,
};
use bitcoin::{
    address::NetworkUnchecked,
    hashes::Hash,
    script::{Instruction, PushBytesBuf},
    taproot::{ControlBlock, Signature as TaprootSignature},
    Address, Amount, Network, ScriptBuf, Transaction, Txid,
};
use zksync_basic_types::H256;
use zksync_types::{Address as EVMAddress, L1BatchNumber};

const MIN_WITNESS_LENGTH: usize = 3;
const VIA_INSCRIPTION_PROTOCOL: &str = "via_inscription_protocol";

pub struct MessageParser {
    network: Network,
}

impl MessageParser {
    pub fn new(network: Network) -> Self {
        Self { network }
    }

    pub fn parse_transaction(&self, tx: &Transaction) -> Vec<Message> {
        tx.input
            .iter()
            .filter_map(|input| self.parse_input(input, tx))
            .collect()
    }

    fn parse_input(&self, input: &bitcoin::TxIn, tx: &Transaction) -> Option<Message> {
        let witness = &input.witness;
        // A valid P2TR input for our inscriptions should have at least 3 witness elements:
        // 1. Signature
        // 2. Inscription script
        // 3. Control block
        if witness.len() < MIN_WITNESS_LENGTH {
            return None;
        }

        let signature = TaprootSignature::from_slice(&witness[0]).ok()?;
        let script = ScriptBuf::from_bytes(witness[1].to_vec());
        let control_block = ControlBlock::decode(&witness[2]).ok()?;

        let instructions: Vec<_> = script.instructions().filter_map(Result::ok).collect();
        let via_index = find_via_inscription_protocol(&instructions)?;

        let public_key = control_block.internal_key;
        let common_fields = CommonFields {
            schnorr_signature: signature,
            encoded_public_key: PushBytesBuf::from(public_key.serialize()),
        };

        self.parse_message(tx, &instructions[via_index..], &common_fields)
    }

    fn parse_message(
        &self,
        tx: &Transaction,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
        let message_type = instructions.get(1)?;

        match message_type {
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::SYSTEM_BOOTSTRAPPING_MSG.as_bytes() =>
            {
                self.parse_system_bootstrapping(instructions, common_fields)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::PROPOSE_SEQUENCER_MSG.as_bytes() =>
            {
                self.parse_propose_sequencer(instructions, common_fields)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::VALIDATOR_ATTESTATION_MSG.as_bytes() =>
            {
                self.parse_validator_attestation(instructions, common_fields)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::L1_BATCH_DA_REFERENCE_MSG.as_bytes() =>
            {
                self.parse_l1_batch_da_reference(instructions, common_fields)
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == types::PROOF_DA_REFERENCE_MSG.as_bytes() =>
            {
                self.parse_proof_da_reference(instructions, common_fields)
            }
            Instruction::PushBytes(bytes) if bytes.as_bytes() == types::L1_TO_L2_MSG.as_bytes() => {
                self.parse_l1_to_l2_message(tx, instructions, common_fields)
            }
            _ => None,
        }
    }

    fn parse_system_bootstrapping(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
        if instructions.len() < 5 {
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

        let verifier_addresses = instructions[3..]
            .iter()
            .take_while(|instr| matches!(instr, Instruction::PushBytes(_)))
            .filter_map(|instr| {
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
            })
            .collect::<Vec<_>>();

        let bridge_address = instructions.last().and_then(|instr| {
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

        Some(Message::SystemBootstrapping(SystemBootstrapping {
            common: common_fields.clone(),
            input: SystemBootstrappingInput {
                start_block_height,
                bridge_p2wpkh_mpc_address: bridge_address,
                verifier_p2wpkh_addresses: verifier_addresses,
            },
        }))
    }

    fn parse_propose_sequencer(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
        if instructions.len() < 3 {
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

        Some(Message::ProposeSequencer(ProposeSequencer {
            common: common_fields.clone(),
            input: ProposeSequencerInput {
                sequencer_new_p2wpkh_address: sequencer_address,
            },
        }))
    }

    fn parse_validator_attestation(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
        if instructions.len() < 4 {
            return None;
        }

        let reference_txid =
            Txid::from_slice(instructions.get(2)?.push_bytes()?.as_bytes()).ok()?;
        let attestation = match instructions.get(3)?.push_bytes()?.as_bytes() {
            b"OP_1" => Vote::Ok,
            b"OP_0" => Vote::NotOk,
            _ => return None,
        };

        Some(Message::ValidatorAttestation(ValidatorAttestation {
            common: common_fields.clone(),
            input: ValidatorAttestationInput {
                reference_txid,
                attestation,
            },
        }))
    }

    fn parse_l1_batch_da_reference(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
        if instructions.len() < 6 {
            return None;
        }

        let l1_batch_hash = H256::from_slice(instructions.get(2)?.push_bytes()?.as_bytes());
        let l1_batch_index = L1BatchNumber(u32::from_be_bytes(
            instructions
                .get(3)?
                .push_bytes()?
                .as_bytes()
                .try_into()
                .ok()?,
        ));
        let da_identifier = std::str::from_utf8(instructions.get(4)?.push_bytes()?.as_bytes())
            .ok()?
            .to_string();
        let blob_id = std::str::from_utf8(instructions.get(5)?.push_bytes()?.as_bytes())
            .ok()?
            .to_string();

        Some(Message::L1BatchDAReference(L1BatchDAReference {
            common: common_fields.clone(),
            input: L1BatchDAReferenceInput {
                l1_batch_hash,
                l1_batch_index,
                da_identifier,
                blob_id,
            },
        }))
    }

    fn parse_proof_da_reference(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
        if instructions.len() < 5 {
            return None;
        }

        let l1_batch_reveal_txid =
            Txid::from_slice(instructions.get(2)?.push_bytes()?.as_bytes()).ok()?;
        let da_identifier = std::str::from_utf8(instructions.get(3)?.push_bytes()?.as_bytes())
            .ok()?
            .to_string();
        let blob_id = std::str::from_utf8(instructions.get(4)?.push_bytes()?.as_bytes())
            .ok()?
            .to_string();

        Some(Message::ProofDAReference(ProofDAReference {
            common: common_fields.clone(),
            input: ProofDAReferenceInput {
                l1_batch_reveal_txid,
                da_identifier,
                blob_id,
            },
        }))
    }

    fn parse_l1_to_l2_message(
        &self,
        tx: &Transaction,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
        if instructions.len() < 5 {
            return None;
        }

        let receiver_l2_address =
            EVMAddress::from_slice(instructions.get(2)?.push_bytes()?.as_bytes());
        let l2_contract_address =
            EVMAddress::from_slice(instructions.get(3)?.push_bytes()?.as_bytes());
        let call_data = instructions.get(4)?.push_bytes()?.as_bytes().to_vec();

        let amount = tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_p2wpkh())
            .map(|output| output.value)
            .unwrap_or(Amount::ZERO);

        Some(Message::L1ToL2Message(L1ToL2Message {
            common: common_fields.clone(),
            amount,
            input: L1ToL2MessageInput {
                receiver_l2_address,
                l2_contract_address,
                call_data,
            },
            tx_outputs: tx.output.clone(), // include all transaction outputs
        }))
    }
}

fn find_via_inscription_protocol(instructions: &[Instruction]) -> Option<usize> {
    // TODO: also check first part of the script (OP_CHECKSIG and other stuff)
    instructions.iter().position(|instr| {
        matches!(instr, Instruction::PushBytes(bytes) if bytes.as_bytes() == VIA_INSCRIPTION_PROTOCOL.as_bytes())
    })
}
