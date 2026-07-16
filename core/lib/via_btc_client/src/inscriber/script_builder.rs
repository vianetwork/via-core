use anyhow::{Context, Result};
use bitcoin::{
    hashes::Hash,
    key::UntweakedPublicKey,
    opcodes::{
        all::{self},
        OP_FALSE, OP_TRUE,
    },
    script::{Builder as ScriptBuilder, PushBytesBuf},
    secp256k1::{Secp256k1, Signing, Verification},
    taproot::{TaprootBuilder, TaprootSpendInfo},
    Address, Network, ScriptBuf,
};
use tracing::{debug, instrument};
use zksync_types::{ethabi::ethereum_types::BigEndianHash, H256};

use crate::types;

pub struct InscriptionData {
    pub inscription_script: ScriptBuf,
    pub script_size: usize,
    pub script_pubkey: ScriptBuf,
    pub taproot_spend_info: TaprootSpendInfo,
}

impl InscriptionData {
    #[instrument(
        skip(inscription_message, secp, internal_key),
        target = "bitcoin_inscriber::script_builder"
    )]
    pub fn new<C: Signing + Verification>(
        inscription_message: &types::InscriptionMessage,
        secp: &Secp256k1<C>,
        internal_key: UntweakedPublicKey,
        network: Network,
    ) -> Result<Self> {
        debug!("Creating new InscriptionData");
        let serialized_pubkey = internal_key.serialize();
        let mut encoded_pubkey = PushBytesBuf::with_capacity(serialized_pubkey.len());
        encoded_pubkey.extend_from_slice(&serialized_pubkey).ok();

        let basic_script = Self::build_basic_inscription_script(&encoded_pubkey)?;

        let (inscription_script, script_size) =
            Self::complete_inscription(basic_script, inscription_message, network)?;

        let (script_pubkey, taproot_spend_info) = Self::construct_inscription_commitment_data(
            secp,
            &inscription_script,
            internal_key,
            network,
        )?;

        debug!("InscriptionData created successfully");
        Ok(Self {
            inscription_script,
            script_size,
            script_pubkey,
            taproot_spend_info,
        })
    }

    #[instrument(
        skip(secp, inscription_script, internal_key),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn construct_inscription_commitment_data<C: Signing + Verification>(
        secp: &Secp256k1<C>,
        inscription_script: &ScriptBuf,
        internal_key: UntweakedPublicKey,
        network: Network,
    ) -> Result<(ScriptBuf, TaprootSpendInfo)> {
        debug!("Constructing inscription commitment data");
        let mut builder = TaprootBuilder::new();
        builder = builder
            .add_leaf(0, inscription_script.clone())
            .with_context(|| "Adding leaf should work")?;

        let taproot_spend_info = builder
            .finalize(secp, internal_key)
            .map_err(|e| anyhow::anyhow!("Failed to finalize taproot spend info: {:?}", e))?;

        let taproot_address = Address::p2tr_tweaked(taproot_spend_info.output_key(), network);

        let script_pubkey = taproot_address.script_pubkey();

        debug!("Inscription commitment data constructed");
        Ok((script_pubkey, taproot_spend_info))
    }

    #[instrument(skip(encoded_pubkey), target = "bitcoin_inscriber::script_builder")]
    fn build_basic_inscription_script(encoded_pubkey: &PushBytesBuf) -> Result<ScriptBuilder> {
        debug!("Building basic inscription script");
        let mut via_prefix_encoded =
            PushBytesBuf::with_capacity(types::VIA_INSCRIPTION_PROTOCOL.len());
        via_prefix_encoded
            .extend_from_slice(types::VIA_INSCRIPTION_PROTOCOL.as_bytes())
            .ok();

        let script = ScriptBuilder::new()
            .push_slice(encoded_pubkey.as_push_bytes())
            .push_opcode(all::OP_CHECKSIG)
            .push_opcode(OP_FALSE)
            .push_opcode(all::OP_IF)
            .push_slice(via_prefix_encoded);

        debug!("Basic inscription script built");
        Ok(script)
    }

    #[instrument(
        skip(basic_script, message),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn complete_inscription(
        basic_script: ScriptBuilder,
        message: &types::InscriptionMessage,
        network: Network,
    ) -> Result<(ScriptBuf, usize)> {
        debug!("Completing inscription for message type: {:?}", message);
        let final_script_result = match message {
            types::InscriptionMessage::L1BatchDAReference(input) => {
                Self::build_l1_batch_da_reference_script(basic_script, input)
            }
            types::InscriptionMessage::ProofDAReference(input) => {
                Self::build_proof_da_reference_script(basic_script, input)
            }
            types::InscriptionMessage::ValidatorAttestation(input) => {
                Self::build_validator_attestation_script(basic_script, input)
            }
            types::InscriptionMessage::SystemBootstrapping(input) => {
                Self::build_system_bootstrapping_script(basic_script, input, network)?
            }
            types::InscriptionMessage::L1ToL2Message(input) => {
                Self::build_l1_to_l2_message_script(basic_script, input)
            }
            types::InscriptionMessage::SystemContractUpgradeProposal(input) => {
                Self::build_system_contract_upgrade_message_script(basic_script, input)
            }
            types::InscriptionMessage::UpdateBridgeProposal(input) => {
                Self::build_update_bridge_script(basic_script, input, network)?
            }
        };

        let final_script = final_script_result.push_opcode(all::OP_ENDIF).into_script();
        let script_size = final_script.len();

        debug!("Inscription completed, script size: {}", script_size);
        Ok((final_script, script_size))
    }

    #[instrument(
        skip(basic_script, input),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn build_l1_batch_da_reference_script(
        basic_script: ScriptBuilder,
        input: &types::L1BatchDAReferenceInput,
    ) -> ScriptBuilder {
        debug!("Building L1BatchDAReference script");
        let l1_batch_hash_encoded = Self::encode_push_bytes(input.l1_batch_hash.as_bytes());
        let l1_batch_index_encoded = Self::encode_push_bytes(&input.l1_batch_index.to_be_bytes());
        let da_identifier_encoded = Self::encode_push_bytes(input.da_identifier.as_bytes());
        let da_reference_encoded = Self::encode_push_bytes(input.blob_id.as_bytes());
        let prev_l1_batch_hash_encoded =
            Self::encode_push_bytes(input.prev_l1_batch_hash.as_bytes());

        basic_script
            .push_slice(&*types::L1_BATCH_DA_REFERENCE_MSG)
            .push_slice(l1_batch_hash_encoded)
            .push_slice(l1_batch_index_encoded)
            .push_slice(da_identifier_encoded)
            .push_slice(da_reference_encoded)
            .push_slice(prev_l1_batch_hash_encoded)
    }

    #[instrument(
        skip(basic_script, input),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn build_proof_da_reference_script(
        basic_script: ScriptBuilder,
        input: &types::ProofDAReferenceInput,
    ) -> ScriptBuilder {
        debug!("Building ProofDAReference script");
        let l1_batch_reveal_txid_encoded =
            Self::encode_push_bytes(input.l1_batch_reveal_txid.as_raw_hash().as_byte_array());
        let da_identifier_encoded = Self::encode_push_bytes(input.da_identifier.as_bytes());
        let da_reference_encoded = Self::encode_push_bytes(input.blob_id.as_bytes());

        basic_script
            .push_slice(&*types::PROOF_DA_REFERENCE_MSG)
            .push_slice(l1_batch_reveal_txid_encoded)
            .push_slice(da_identifier_encoded)
            .push_slice(da_reference_encoded)
    }

    #[instrument(
        skip(basic_script, input),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn build_validator_attestation_script(
        basic_script: ScriptBuilder,
        input: &types::ValidatorAttestationInput,
    ) -> ScriptBuilder {
        debug!("Building ValidatorAttestation script");
        let reference_txid_encoded =
            Self::encode_push_bytes(input.reference_txid.as_raw_hash().as_byte_array());

        let script = basic_script
            .push_slice(&*types::VALIDATOR_ATTESTATION_MSG)
            .push_slice(reference_txid_encoded);

        match input.attestation {
            types::Vote::Ok => script.push_opcode(OP_TRUE),
            types::Vote::NotOk => script.push_opcode(OP_FALSE),
        }
    }

    #[instrument(
        skip(basic_script, input),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn build_system_bootstrapping_script(
        basic_script: ScriptBuilder,
        input: &types::SystemBootstrappingInput,
        network: Network,
    ) -> Result<ScriptBuilder> {
        debug!("Building SystemBootstrapping script");
        let start_block_height_encoded =
            Self::encode_push_bytes(&input.start_block_height.to_be_bytes());

        let bridge_address = input
            .bridge_musig2_address
            .clone()
            .require_network(network)?;
        let bridge_address_encoded = Self::encode_push_bytes(bridge_address.to_string().as_bytes());

        let sequencer_address = input.sequencer_address.clone().require_network(network)?;

        let sequencer_address_encoded =
            Self::encode_push_bytes(sequencer_address.to_string().as_bytes());

        let bootloader_hash = Self::encode_push_bytes(input.bootloader_hash.as_bytes());

        let abstract_account_hash = Self::encode_push_bytes(input.abstract_account_hash.as_bytes());

        let governance_address = input.governance_address.clone().require_network(network)?;
        let governance_address_encoded =
            Self::encode_push_bytes(governance_address.to_string().as_bytes());

        let protocol_version =
            Self::encode_push_bytes(H256::from_uint(&input.protocol_version.pack()).as_bytes());

        let snark_wrapper_vk_hash = Self::encode_push_bytes(input.snark_wrapper_vk_hash.as_bytes());

        let evm_emulator_hash = Self::encode_push_bytes(input.evm_emulator_hash.as_bytes());

        let mut script = basic_script.push_slice(&*types::SYSTEM_BOOTSTRAPPING_MSG);
        script = script
            .push_slice(start_block_height_encoded)
            .push_slice(protocol_version)
            .push_slice(bootloader_hash)
            .push_slice(abstract_account_hash)
            .push_slice(snark_wrapper_vk_hash)
            .push_slice(evm_emulator_hash)
            .push_slice(governance_address_encoded)
            .push_slice(sequencer_address_encoded)
            .push_slice(bridge_address_encoded);

        for verifier_p2wpkh_address in &input.verifier_p2wpkh_addresses {
            let network_checked_address =
                verifier_p2wpkh_address.clone().require_network(network)?;
            let address_encoded =
                Self::encode_push_bytes(network_checked_address.to_string().as_bytes());
            script = script.push_slice(address_encoded);
        }

        Ok(script)
    }

    #[instrument(
        skip(basic_script, input),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn build_l1_to_l2_message_script(
        basic_script: ScriptBuilder,
        input: &types::L1ToL2MessageInput,
    ) -> ScriptBuilder {
        debug!("Building L1ToL2Message script");
        let receiver_l2_address_encoded =
            Self::encode_push_bytes(input.receiver_l2_address.as_bytes());
        let l2_contract_address_encoded =
            Self::encode_push_bytes(input.l2_contract_address.as_bytes());
        let call_data_encoded = Self::encode_push_bytes(&input.call_data);

        basic_script
            .push_slice(&*types::L1_TO_L2_MSG)
            .push_slice(receiver_l2_address_encoded)
            .push_slice(l2_contract_address_encoded)
            .push_slice(call_data_encoded)
    }

    #[instrument(
        skip(basic_script, input),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn build_system_contract_upgrade_message_script(
        basic_script: ScriptBuilder,
        input: &types::SystemContractUpgradeProposalInput,
    ) -> ScriptBuilder {
        debug!("Building SystemContract script");

        let version_encoded =
            Self::encode_push_bytes(H256::from_uint(&input.version.pack()).as_bytes());
        let bootloader_code_hash_encoded =
            Self::encode_push_bytes(input.bootloader_code_hash.as_bytes());
        let default_account_code_hash_encoded =
            Self::encode_push_bytes(input.default_account_code_hash.as_bytes());
        let recursion_scheduler_level_vk_hash_encoded =
            Self::encode_push_bytes(input.recursion_scheduler_level_vk_hash.as_bytes());

        let mut basic_script = basic_script;
        basic_script = basic_script
            .push_slice(&*types::SYSTEM_CONTRACT_UPGRADE_MSG)
            .push_slice(version_encoded)
            .push_slice(bootloader_code_hash_encoded)
            .push_slice(default_account_code_hash_encoded)
            .push_slice(recursion_scheduler_level_vk_hash_encoded);

        for (address, hash) in &input.system_contracts {
            basic_script = basic_script.push_slice(Self::encode_push_bytes(address.as_bytes()));
            basic_script = basic_script.push_slice(Self::encode_push_bytes(hash.as_bytes()));
        }
        basic_script
    }

    #[instrument(
        skip(basic_script, input),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn build_update_bridge_script(
        basic_script: ScriptBuilder,
        input: &types::UpdateBridgeProposalInput,
        network: Network,
    ) -> anyhow::Result<ScriptBuilder> {
        debug!("Building Bridge address script");

        let mut script = basic_script.push_slice(&*types::UPGRADE_BRIDGE_MSG);

        for verifier_p2wpkh_address in &input.verifier_p2wpkh_addresses {
            let network_checked_address =
                verifier_p2wpkh_address.clone().require_network(network)?;
            let address_encoded =
                Self::encode_push_bytes(network_checked_address.to_string().as_bytes());
            script = script.push_slice(address_encoded);
        }

        let bridge_address = input
            .bridge_musig2_address
            .clone()
            .require_network(network)?;
        let bridge_address_encoded = Self::encode_push_bytes(bridge_address.to_string().as_bytes());

        Ok(script.push_slice(bridge_address_encoded))
    }

    #[instrument(skip(data), target = "bitcoin_inscriber::script_builder")]
    fn encode_push_bytes(data: &[u8]) -> PushBytesBuf {
        let mut encoded = PushBytesBuf::with_capacity(data.len());
        encoded.extend_from_slice(data).ok();
        encoded
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::{
        absolute,
        secp256k1::{Keypair, Secp256k1, SecretKey},
        taproot::{LeafVersion, Signature as TaprootSignature, TaprootBuilder},
        transaction, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
        Witness,
    };
    use zksync_types::{
        protocol_version::{ProtocolSemanticVersion, VersionPatch},
        Address as EVMAddress, ProtocolVersionId, H256,
    };

    use super::*;
    use crate::{
        indexer::MessageParser,
        types::{FullInscriptionMessage, InscriptionMessage, SystemContractUpgradeProposalInput},
    };

    fn parse_system_contract_upgrade_script<C: Signing + Verification>(
        secp: &Secp256k1<C>,
        internal_key: UntweakedPublicKey,
        public_key: &bitcoin::PublicKey,
        script: ScriptBuf,
    ) -> Vec<FullInscriptionMessage> {
        let spend_info = TaprootBuilder::new()
            .add_leaf(0, script.clone())
            .unwrap()
            .finalize(secp, internal_key)
            .unwrap();
        let control_block = spend_info
            .control_block(&(script.clone(), LeafVersion::TapScript))
            .unwrap();
        let reveal_witness = Witness::from_slice(&[
            TaprootSignature::from_slice(&[0; 64]).unwrap().to_vec(),
            script.into_bytes(),
            control_block.serialize(),
        ]);
        let sender_witness = Witness::from_slice(&[Vec::new(), public_key.to_bytes()]);
        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![
                TxIn {
                    previous_output: OutPoint::null(),
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: reveal_witness,
                },
                TxIn {
                    previous_output: OutPoint::null(),
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: sender_witness,
                },
            ],
            output: vec![TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::new(),
            }],
        };

        MessageParser::new(Network::Regtest).parse_system_transaction(&tx, 42, None)
    }

    #[test]
    fn system_contract_upgrade_proposal_builder_round_trips_all_contracts() {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(&[1; 32]).unwrap();
        let keypair = Keypair::from_secret_key(&secp, &secret_key);
        let (internal_key, _) = keypair.x_only_public_key();
        let public_key = bitcoin::PublicKey::new(bitcoin::secp256k1::PublicKey::from_secret_key(
            &secp,
            &secret_key,
        ));

        for pair_count in [0, 3, 4, 7] {
            let input = SystemContractUpgradeProposalInput {
                version: ProtocolSemanticVersion::new(
                    ProtocolVersionId::Version28,
                    VersionPatch(3),
                ),
                bootloader_code_hash: H256::repeat_byte(0x11),
                default_account_code_hash: H256::repeat_byte(0x22),
                evm_emulator_code_hash: None,
                recursion_scheduler_level_vk_hash: H256::repeat_byte(0x33),
                system_contracts: (0..pair_count)
                    .map(|index| {
                        let byte = index as u8 + 1;
                        (
                            EVMAddress::repeat_byte(byte),
                            H256::repeat_byte(byte + 0x40),
                        )
                    })
                    .collect(),
            };
            let inscription = InscriptionData::new(
                &InscriptionMessage::SystemContractUpgradeProposal(input.clone()),
                &secp,
                internal_key,
                Network::Regtest,
            )
            .unwrap();
            let messages = parse_system_contract_upgrade_script(
                &secp,
                internal_key,
                &public_key,
                inscription.inscription_script,
            );
            let [FullInscriptionMessage::SystemContractUpgradeProposal(message)] =
                messages.as_slice()
            else {
                panic!("expected one system-contract upgrade proposal");
            };
            assert_eq!(message.input, input, "pair count {pair_count}");
        }

        let input = SystemContractUpgradeProposalInput {
            version: ProtocolSemanticVersion::new(ProtocolVersionId::Version28, VersionPatch(4)),
            bootloader_code_hash: H256::repeat_byte(0x44),
            default_account_code_hash: H256::repeat_byte(0x55),
            evm_emulator_code_hash: None,
            recursion_scheduler_level_vk_hash: H256::repeat_byte(0x66),
            system_contracts: vec![],
        };
        let script = ScriptBuilder::new()
            .push_slice(InscriptionData::encode_push_bytes(
                types::VIA_INSCRIPTION_PROTOCOL.as_bytes(),
            ))
            .push_slice(&*types::SYSTEM_CONTRACT_UPGRADE_MSG)
            .push_slice(InscriptionData::encode_push_bytes(
                H256::from_uint(&input.version.pack()).as_bytes(),
            ))
            .push_slice(InscriptionData::encode_push_bytes(
                input.bootloader_code_hash.as_bytes(),
            ))
            .push_slice(InscriptionData::encode_push_bytes(
                input.default_account_code_hash.as_bytes(),
            ))
            .push_slice(InscriptionData::encode_push_bytes(
                input.recursion_scheduler_level_vk_hash.as_bytes(),
            ))
            .into_script();
        assert_eq!(script.instructions().count(), 6);

        let messages =
            parse_system_contract_upgrade_script(&secp, internal_key, &public_key, script);
        let [FullInscriptionMessage::SystemContractUpgradeProposal(message)] = messages.as_slice()
        else {
            panic!("expected crafted zero-pair proposal");
        };
        assert_eq!(message.input, input);
    }
}
