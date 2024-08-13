use async_trait::async_trait;
use bitcoin::{
    address::NetworkUnchecked,
    hashes::Hash,
    script::{Instruction, PushBytesBuf},
    secp256k1::XOnlyPublicKey,
    taproot::Signature,
    Address, Amount, BlockHash, KnownHrp, Network, ScriptBuf, Transaction,
};
pub use bitcoin::{Network as BitcoinNetwork, Txid};
use bitcoincore_rpc::Auth;
use zksync_basic_types::H256;
use zksync_types::{Address as EVMAddress, L1BatchNumber};

use crate::{
    client::BitcoinClient,
    traits::BitcoinIndexerOpt,
    types::{
        BitcoinError, BitcoinIndexerResult, CommonFields, L1BatchDAReference,
        L1BatchDAReferenceInput, L1ToL2Message, L1ToL2MessageInput, Message, ProofDAReference,
        ProofDAReferenceInput, ProposeSequencer, ProposeSequencerInput, SystemBootstrapping,
        SystemBootstrappingInput, ValidatorAttestation, ValidatorAttestationInput, Vote,
    },
    BitcoinOps,
};

pub struct BitcoinInscriptionIndexer {
    client: Box<dyn BitcoinOps>,
    bridge_address: Option<Address>,
    verifier_addresses: Vec<Address>,
    starting_block_number: u32,
    sequencer_address: Option<Address>,
}

#[async_trait]
impl BitcoinIndexerOpt for BitcoinInscriptionIndexer {
    async fn new(rpc_url: &str, network: BitcoinNetwork, txid: &Txid) -> BitcoinIndexerResult<Self>
    where
        Self: Sized,
    {
        let client = Box::new(BitcoinClient::new(rpc_url, network, Auth::None).await?);
        let tx = client.get_rpc_client().get_transaction(txid).await?;

        let temp_indexer = Self {
            client,
            bridge_address: None,
            sequencer_address: None,
            verifier_addresses: Vec::new(),
            starting_block_number: 0,
        };

        let mut init_msgs = temp_indexer
            .process_tx(&tx)
            .ok_or_else(|| BitcoinError::Other("Failed to process transaction".to_string()))?;

        if let Some(Message::SystemBootstrapping(system_bootstrapping)) = init_msgs.pop() {
            let client = Box::new(BitcoinClient::new(rpc_url, network, Auth::None).await?);
            Ok(Self {
                client,
                bridge_address: Some(system_bootstrapping.input.bridge_p2wpkh_mpc_address),
                verifier_addresses: system_bootstrapping.input.verifier_addresses,
                starting_block_number: system_bootstrapping.input.start_block_height,
                sequencer_address: None,
            })
        } else {
            Err(BitcoinError::Other(
                "Indexer error: provided txid does not contain SystemBootstrapping message"
                    .to_string(),
            ))
        }
    }

    async fn process_blocks(
        &self,
        starting_block: u32,
        ending_block: u32,
    ) -> BitcoinIndexerResult<Vec<Message>> {
        let mut res = Vec::with_capacity((ending_block - starting_block + 1) as usize);
        for block in starting_block..=ending_block {
            res.extend(self.process_block(block).await?);
        }
        Ok(res)
    }

    async fn process_block(&self, block: u32) -> BitcoinIndexerResult<Vec<Message>> {
        if block < self.starting_block_number {
            return Err(BitcoinError::Other(
                "Indexer error: can't get block before starting block".to_string(),
            ));
        }

        let block = self
            .client
            .get_rpc_client()
            .get_block_by_height(block as u128)
            .await?;
        let res: Vec<_> = block
            .txdata
            .iter()
            .filter_map(|tx| self.process_tx(tx))
            .flatten()
            .collect();
        Ok(res)
    }

    async fn are_blocks_connected(
        &self,
        parent_hash: &BlockHash,
        child_hash: &BlockHash,
    ) -> BitcoinIndexerResult<bool> {
        let child_block = self
            .client
            .get_rpc_client()
            .get_block_by_hash(child_hash)
            .await?;
        Ok(child_block.header.prev_blockhash == *parent_hash)
    }
}

impl BitcoinInscriptionIndexer {
    fn process_tx(&self, tx: &Transaction) -> Option<Vec<Message>> {
        let mut messages = Vec::new();

        for (input_index, input) in tx.input.iter().enumerate() {
            let witness = &input.witness;
            if witness.len() < 3 {
                continue;
            }

            let signature = Signature::from_slice(&witness[0]).ok()?;
            let public_key = XOnlyPublicKey::from_slice(&witness[1]).ok()?;
            let script = ScriptBuf::from_bytes(witness.last()?.to_vec());

            let instructions: Vec<_> = script.instructions().filter_map(Result::ok).collect();
            let via_index = is_via_inscription_protocol(&instructions)?;

            let common_fields = CommonFields {
                schnorr_signature: signature,
                encoded_public_key: PushBytesBuf::from(public_key.serialize()),
                via_inscription_protocol_identifier: "via_inscription_protocol".to_string(),
            };

            if let Some(message) =
                self.parse_message(input_index, tx, &instructions[via_index..], &common_fields)
            {
                messages.push(message);
            }
        }

        if messages.is_empty() {
            None
        } else {
            Some(messages)
        }
    }

    fn parse_system_bootstrapping(
        &self,
        instructions: &[Instruction],
        common_fields: CommonFields,
    ) -> Option<Message> {
        if instructions.len() < 11 {
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

        let mut verifier_addresses = Vec::new();
        for i in 3..10 {
            if let Some(Instruction::PushBytes(bytes)) = instructions.get(i) {
                if let Ok(address_str) = std::str::from_utf8(bytes.as_bytes()) {
                    if let Ok(address) = address_str.parse::<Address<NetworkUnchecked>>() {
                        if let Ok(network_address) =
                            address.require_network(self.client.get_network())
                        {
                            verifier_addresses.push(network_address);
                            continue;
                        }
                    }
                }
                break;
            } else {
                break;
            }
        }

        let bridge_address = match instructions.get(10)?.push_bytes() {
            Some(bytes) => {
                if let Ok(address_str) = std::str::from_utf8(bytes.as_bytes()) {
                    address_str
                        .parse::<Address<NetworkUnchecked>>()
                        .ok()?
                        .require_network(self.client.get_network())
                        .ok()?
                } else {
                    return None;
                }
            }
            None => return None,
        };

        Some(Message::SystemBootstrapping(SystemBootstrapping {
            common: common_fields,
            input: SystemBootstrappingInput {
                start_block_height,
                verifier_addresses,
                bridge_p2wpkh_mpc_address: bridge_address,
            },
        }))
    }

    fn parse_propose_sequencer(
        &self,
        instructions: &[Instruction],
        common_fields: CommonFields,
    ) -> Option<Message> {
        if instructions.len() < 3 {
            return None;
        }

        let proposer_address = match instructions.get(2)?.push_bytes() {
            Some(bytes) => {
                if let Ok(address_str) = std::str::from_utf8(bytes.as_bytes()) {
                    address_str
                        .parse::<Address<NetworkUnchecked>>()
                        .ok()?
                        .require_network(self.client.get_network())
                        .ok()?
                } else {
                    return None;
                }
            }
            None => return None,
        };

        Some(Message::ProposeSequencer(ProposeSequencer {
            common: common_fields,
            input: ProposeSequencerInput {
                sequencer_p2wpkh_address: proposer_address,
            },
        }))
    }

    fn parse_validator_attestation(
        &self,
        instructions: &[Instruction],
        common_fields: CommonFields,
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
            common: common_fields,
            input: ValidatorAttestationInput {
                reference_txid,
                attestation,
            },
        }))
    }

    fn parse_proof_da_reference(
        &self,
        instructions: &[Instruction],
        common_fields: CommonFields,
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
            common: common_fields,
            input: ProofDAReferenceInput {
                l1_batch_reveal_txid,
                da_identifier,
                blob_id,
            },
        }))
    }

    fn parse_l1_batch_da_reference(
        &self,
        instructions: &[Instruction],
        common_fields: CommonFields,
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
            common: common_fields,
            input: L1BatchDAReferenceInput {
                l1_batch_hash,
                l1_batch_index,
                da_identifier,
                blob_id,
            },
        }))
    }

    fn parse_l1_to_l2_message(
        &self,
        _input_index: usize,
        tx: &Transaction,
        instructions: &[Instruction],
        common_fields: CommonFields,
    ) -> Option<Message> {
        if instructions.len() < 5 {
            return None;
        }

        let receiver_l2_address =
            EVMAddress::from_slice(instructions.get(2)?.push_bytes()?.as_bytes());
        let l2_contract_address =
            EVMAddress::from_slice(instructions.get(3)?.push_bytes()?.as_bytes());
        let call_data = instructions.get(4)?.push_bytes()?.as_bytes().to_vec();

        let is_bridging = l2_contract_address == EVMAddress::zero() && call_data.is_empty();

        let amount = if is_bridging {
            tx.output
                .iter()
                .find(|output| {
                    Some(&output.script_pubkey)
                        == self
                            .bridge_address
                            .as_ref()
                            .map(|addr| addr.script_pubkey())
                            .as_ref()
                })
                .map(|output| output.value)
                .unwrap_or(Amount::ZERO)
        } else {
            Amount::ZERO
        };

        Some(Message::L1ToL2Message(L1ToL2Message {
            common: common_fields,
            amount,
            input: L1ToL2MessageInput {
                receiver_l2_address,
                l2_contract_address,
                call_data,
            },
        }))
    }

    fn parse_message(
        &self,
        input_index: usize,
        tx: &Transaction,
        instructions: &[Instruction],
        common_fields: &CommonFields,
    ) -> Option<Message> {
        let message_type = instructions.get(1)?;
        let hrp = match self.client.get_network() {
            Network::Bitcoin => KnownHrp::Mainnet,
            Network::Testnet | Network::Signet => KnownHrp::Testnets,
            Network::Regtest => KnownHrp::Regtest,
            _ => return None, // TODO: why do we need that here?
        };
        let sender_address = Address::p2tr(
            &bitcoin::secp256k1::Secp256k1::new(),
            XOnlyPublicKey::from_slice(&common_fields.encoded_public_key.as_bytes()).ok()?,
            None,
            hrp,
        );

        match message_type {
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == b"Str('SystemBootstrappingMessage')" =>
            {
                if self.starting_block_number != 0 {
                    return None;
                }
                self.parse_system_bootstrapping(instructions, common_fields.clone())
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == b"Str('ProposeSequencerMessage')" =>
            {
                if !self.verifier_addresses.contains(&sender_address) {
                    return None;
                }
                self.parse_propose_sequencer(instructions, common_fields.clone())
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == b"Str('ValidatorAttestationMessage')" =>
            {
                if !self.verifier_addresses.contains(&sender_address) {
                    return None;
                }
                self.parse_validator_attestation(instructions, common_fields.clone())
            }
            Instruction::PushBytes(bytes) if bytes.as_bytes() == b"Str('L1BatchDAReference')" => {
                if Some(&sender_address) != self.sequencer_address.as_ref() {
                    return None;
                }
                self.parse_l1_batch_da_reference(instructions, common_fields.clone())
            }
            Instruction::PushBytes(bytes)
                if bytes.as_bytes() == b"Str('ProofDAReferenceMessage')" =>
            {
                if Some(&sender_address) != self.sequencer_address.as_ref() {
                    return None;
                }
                self.parse_proof_da_reference(instructions, common_fields.clone())
            }
            Instruction::PushBytes(bytes) if bytes.as_bytes() == b"Str('L1ToL2Message')" => {
                self.parse_l1_to_l2_message(input_index, tx, instructions, common_fields.clone())
            }
            _ => None,
        }
    }
}

fn is_via_inscription_protocol(instructions: &[Instruction]) -> Option<usize> {
    instructions.iter().position(|instr| {
        matches!(instr, Instruction::PushBytes(bytes) if bytes.as_bytes() == b"Str('via_inscription_protocol')")
    })
}
