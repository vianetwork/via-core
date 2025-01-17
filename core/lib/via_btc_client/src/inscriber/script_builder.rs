use anyhow::{Context, Result};
use bitcoin::{
    hashes::Hash,
    key::UntweakedPublicKey,
    opcodes::{all, OP_0, OP_FALSE},
    script::{Builder as ScriptBuilder, PushBytesBuf},
    secp256k1::{Secp256k1, Signing, Verification},
    taproot::{TaprootBuilder, TaprootSpendInfo},
    Address, Network, ScriptBuf,
};
use tracing::{debug, instrument};

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
            .context("adding leaf should work")?;

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
            types::InscriptionMessage::ProposeSequencer(input) => {
                Self::build_propose_sequencer_script(basic_script, input, network)?
            }
            types::InscriptionMessage::L1ToL2Message(input) => {
                Self::build_l1_to_l2_message_script(basic_script, input)
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

        basic_script
            .push_slice(&*types::L1_BATCH_DA_REFERENCE_MSG)
            .push_slice(l1_batch_hash_encoded)
            .push_slice(l1_batch_index_encoded)
            .push_slice(da_identifier_encoded)
            .push_slice(da_reference_encoded)
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
            types::Vote::Ok => script.push_opcode(all::OP_PUSHNUM_1),
            types::Vote::NotOk => script.push_opcode(OP_0),
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

        let mut script = basic_script.push_slice(&*types::SYSTEM_BOOTSTRAPPING_MSG);
        script = script.push_slice(start_block_height_encoded);

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

        let boostloader_hash = Self::encode_push_bytes(input.bootloader_hash.as_bytes());

        let abstract_account_hash = Self::encode_push_bytes(input.abstract_account_hash.as_bytes());

        Ok(script
            .push_slice(bridge_address_encoded)
            .push_slice(boostloader_hash)
            .push_slice(abstract_account_hash))
    }

    #[instrument(
        skip(basic_script, input),
        target = "bitcoin_inscriber::script_builder"
    )]
    fn build_propose_sequencer_script(
        basic_script: ScriptBuilder,
        input: &types::ProposeSequencerInput,
        network: Network,
    ) -> Result<ScriptBuilder> {
        debug!("Building ProposeSequencer script");

        let address = input
            .sequencer_new_p2wpkh_address
            .clone()
            .require_network(network)?;
        let address_encoded = Self::encode_push_bytes(address.to_string().as_bytes());

        Ok(basic_script
            .push_slice(&*types::PROPOSE_SEQUENCER_MSG)
            .push_slice(address_encoded))
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

    #[instrument(skip(data), target = "bitcoin_inscriber::script_builder")]
    fn encode_push_bytes(data: &[u8]) -> PushBytesBuf {
        let mut encoded = PushBytesBuf::with_capacity(data.len());
        encoded.extend_from_slice(data).ok();
        encoded
    }
}
