use bitcoin::{
    address::NetworkUnchecked,
    hashes::Hash,
    key::UntweakedPublicKey,
    script::{Instruction, PushBytesBuf},
    taproot::{ControlBlock, Signature as TaprootSignature},
    Address, Amount, CompressedPublicKey, Network, ScriptBuf, Transaction, Txid,
};
use secp256k1::{Parity, PublicKey};
use tracing::{debug, instrument, warn};
use zksync_basic_types::H256;
use zksync_types::{Address as EVMAddress, L1BatchNumber};

use crate::{
    types,
    types::{
        CommonFields, FullInscriptionMessage as Message, L1BatchDAReference,
        L1BatchDAReferenceInput, L1ToL2Message, L1ToL2MessageInput, ProofDAReference,
        ProofDAReferenceInput, ProposeSequencer, ProposeSequencerInput, SystemBootstrapping,
        SystemBootstrappingInput, ValidatorAttestation, ValidatorAttestationInput, Vote,
    },
};

// Using constants to define the minimum number of instructions can help to make parsing more quick
const MIN_WITNESS_LENGTH: usize = 3;
const MIN_SYSTEM_BOOTSTRAPPING_INSTRUCTIONS: usize = 7;
const MIN_PROPOSE_SEQUENCER_INSTRUCTIONS: usize = 3;
const MIN_VALIDATOR_ATTESTATION_INSTRUCTIONS: usize = 4;
const MIN_L1_BATCH_DA_REFERENCE_INSTRUCTIONS: usize = 6;
const MIN_PROOF_DA_REFERENCE_INSTRUCTIONS: usize = 5;
const MIN_L1_TO_L2_MESSAGE_INSTRUCTIONS: usize = 5;

#[derive(Debug)]
pub struct MessageParser {
    network: Network,
}

impl MessageParser {
    pub fn new(network: Network) -> Self {
        Self { network }
    }

    #[instrument(skip(self, tx), target = "bitcoin_indexer::parser")]
    pub fn parse_transaction(&self, tx: &Transaction, block_height: u32) -> Vec<Message> {
        debug!("Parsing transaction");
        tx.input
            .iter()
            .filter_map(|input| self.parse_input(input, tx, block_height))
            .collect()
    }

    #[instrument(skip(self, input, tx), target = "bitcoin_indexer::parser")]
    fn parse_input(
        &self,
        input: &bitcoin::TxIn,
        tx: &Transaction,
        block_height: u32,
    ) -> Option<Message> {
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
        };

        self.parse_message(tx, &instructions[via_index..], &common_fields)
    }

    #[instrument(
        skip(self, tx, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
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
            Instruction::PushBytes(bytes) => {
                warn!("Unknown message type");
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
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
        if instructions.len() < MIN_SYSTEM_BOOTSTRAPPING_INSTRUCTIONS {
            warn!("Insufficient instructions for system bootstrapping");
            return None;
        }

        let height = u32::from_be_bytes(
            instructions
                .get(2)?
                .push_bytes()?
                .as_bytes()
                .try_into()
                .ok()?,
        );
        let start_block_height = {
            debug!("Parsed start block height: {}", height);
            height
        };

        let verifier_addresses = instructions[3..instructions.len() - 4]
            .iter()
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

        debug!("Parsed {} verifier addresses", verifier_addresses.len());

        let bridge_address = instructions.get(instructions.len() - 4).and_then(|instr| {
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

        debug!("Parsed bridge address");

        // we need this to enable serde serialization and deserialization on the library structs
        let network_unchecked_verifier_addresses: Vec<Address<NetworkUnchecked>> =
            verifier_addresses
                .iter()
                .map(|a| a.as_unchecked().clone())
                .collect();

        let bootloader_hash = H256::from_slice(
            instructions
                .get(instructions.len() - 3)?
                .push_bytes()?
                .as_bytes(),
        );

        debug!("Parsed bootloader hash");

        let abstract_account_hash = H256::from_slice(
            instructions
                .get(instructions.len() - 2)?
                .push_bytes()?
                .as_bytes(),
        );

        debug!("Parsed abstract account hash");

        Some(Message::SystemBootstrapping(SystemBootstrapping {
            common: common_fields.clone(),
            input: SystemBootstrappingInput {
                start_block_height,
                bridge_p2wpkh_mpc_address: bridge_address.as_unchecked().clone(),
                verifier_p2wpkh_addresses: network_unchecked_verifier_addresses,
                bootloader_hash,
                abstract_account_hash,
            },
        }))
    }

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_propose_sequencer(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
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

        Some(Message::ProposeSequencer(ProposeSequencer {
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
    ) -> Option<Message> {
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
                b"OP_0" => Vote::NotOk,
                _ => {
                    warn!("Invalid attestation value");
                    return None;
                }
            },
            Instruction::Op(op) => {
                if op.to_u8() == 0x51 {
                    Vote::Ok
                } else if op.to_u8() == 0x50 {
                    // TODO: check this variant
                    Vote::NotOk
                } else {
                    warn!("Invalid attestation value");
                    return None;
                }
            }
        };

        debug!("Parsed attestation: {:?}", attestation);

        Some(Message::ValidatorAttestation(ValidatorAttestation {
            common: common_fields.clone(),
            input: ValidatorAttestationInput {
                reference_txid,
                attestation,
            },
        }))
    }

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_l1_batch_da_reference(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
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

    #[instrument(
        skip(self, instructions, common_fields),
        target = "bitcoin_indexer::parser"
    )]
    fn parse_proof_da_reference(
        &self,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
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

        Some(Message::ProofDAReference(ProofDAReference {
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
    ) -> Option<Message> {
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
            .find(|output| output.script_pubkey.is_p2wpkh())
            .map(|output| output.value)
            .unwrap_or(Amount::ZERO);
        debug!("Parsed amount: {}", amount);

        Some(Message::L1ToL2Message(L1ToL2Message {
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

pub fn get_btc_address(common_fields: &CommonFields, network: Network) -> Option<Address> {
    let internal_pubkey =
        UntweakedPublicKey::from_slice(&common_fields.encoded_public_key.as_bytes()).ok()?;
    let internal_pubkey = PublicKey::from_x_only_public_key(internal_pubkey, Parity::Even);
    let compressed_pubkey = CompressedPublicKey::from_slice(&internal_pubkey.serialize()).unwrap();

    let address = Address::p2wpkh(&compressed_pubkey, network);

    Some(address)
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
        let parser = MessageParser::new(network);
        let tx = setup_test_transaction();

        let messages = parser.parse_transaction(&tx, 0);
        assert_eq!(messages.len(), 1);
    }

    #[ignore]
    #[test]
    fn test_parse_system_bootstrapping() {
        let network = Network::Bitcoin;
        let parser = MessageParser::new(network);
        let tx = setup_test_transaction();

        if let Some(Message::SystemBootstrapping(bootstrapping)) =
            parser.parse_transaction(&tx, 0).pop()
        {
            assert_eq!(bootstrapping.input.start_block_height, 10);
            assert_eq!(bootstrapping.input.verifier_p2wpkh_addresses.len(), 1);
        } else {
            panic!("Expected SystemBootstrapping message");
        }
    }
}
