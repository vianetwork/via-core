// Please checkout ../../Dev.md file Taproot Script section for more details on the via_inscription_protocol structure and message types

// use crate::inscriber::types;
use crate::types;
use anyhow::{Context, Result};
use bitcoin::hashes::Hash;
use bitcoin::opcodes::{all, OP_0, OP_FALSE};
use bitcoin::script::{Builder as ScriptBuilder, PushBytesBuf};
use bitcoin::secp256k1::{Secp256k1, Signing, Verification};
use bitcoin::taproot::TaprootBuilder;
use bitcoin::{key::UntweakedPublicKey, taproot::TaprootSpendInfo, ScriptBuf};
use bitcoin::{Address, Network};

const VIA_INSCRIPTION_PROTOCOL: &str = "via_inscription_protocol";

pub struct InscriptionData {
    pub inscription_script: ScriptBuf,
    pub script_size: usize,
    pub script_pubkey: ScriptBuf,
    pub taproot_spend_info: TaprootSpendInfo,
}

impl InscriptionData {
    pub fn new<C: Signing + Verification>(
        inscription_message: types::InscriptionMessage,
        secp: &Secp256k1<C>,
        internal_key: UntweakedPublicKey,
        network: Network,
    ) -> Result<Self> {
        let serialized_pubkey = internal_key.serialize();
        let mut encoded_pubkey = PushBytesBuf::with_capacity(serialized_pubkey.len());
        encoded_pubkey.extend_from_slice(&serialized_pubkey).ok();

        let basic_script = Self::build_basic_inscription_script(&encoded_pubkey)?;

        let (inscription_script, script_size) =
            Self::complete_inscription(basic_script, inscription_message)?;

        let (script_pubkey, taproot_spend_info) = Self::construct_inscription_commitment_data(
            secp,
            &inscription_script,
            internal_key,
            network,
        )?;

        let res = Self {
            inscription_script,
            script_size,
            script_pubkey,
            taproot_spend_info,
        };

        Ok(res)
    }

    fn construct_inscription_commitment_data<C: Signing + Verification>(
        secp: &Secp256k1<C>,
        inscription_script: &ScriptBuf,
        internal_key: UntweakedPublicKey,
        network: Network,
    ) -> Result<(ScriptBuf, TaprootSpendInfo)> {
        let mut builder = TaprootBuilder::new();
        builder = builder
            .add_leaf(0, inscription_script.clone())
            .context("adding leaf should work")?;

        let taproot_spend_info = builder
            .finalize(secp, internal_key)
            .map_err(|e| anyhow::anyhow!("Failed to finalize taproot spend info: {:?}", e))?;

        let taproot_address = Address::p2tr_tweaked(taproot_spend_info.output_key(), network);

        let script_pubkey = taproot_address.script_pubkey();

        Ok((script_pubkey, taproot_spend_info))
    }

    fn build_basic_inscription_script(encoded_pubkey: &PushBytesBuf) -> Result<ScriptBuilder> {
        let mut via_prefix_encoded = PushBytesBuf::with_capacity(VIA_INSCRIPTION_PROTOCOL.len());
        via_prefix_encoded
            .extend_from_slice(VIA_INSCRIPTION_PROTOCOL.as_bytes())
            .ok();

        let script = ScriptBuilder::new()
            .push_slice(encoded_pubkey.as_push_bytes())
            .push_opcode(all::OP_CHECKSIG)
            .push_opcode(OP_FALSE)
            .push_opcode(all::OP_IF)
            .push_slice(via_prefix_encoded);

        Ok(script)
    }

    fn complete_inscription(
        basic_script: ScriptBuilder,
        message: types::InscriptionMessage,
    ) -> Result<(ScriptBuf, usize)> {
        let final_script_result: ScriptBuilder;

        match message {
            types::InscriptionMessage::L1BatchDAReference(input) => {
                let l1_batch_hash_bytes = input.l1_batch_hash.as_bytes();
                let mut l1_batch_hash_encoded =
                    PushBytesBuf::with_capacity(l1_batch_hash_bytes.len());
                l1_batch_hash_encoded
                    .extend_from_slice(l1_batch_hash_bytes)
                    .ok();

                let l1_batch_index_bytes = input.l1_batch_index.to_be_bytes();
                let mut l1_batch_index_encoded =
                    PushBytesBuf::with_capacity(l1_batch_index_bytes.len());
                l1_batch_index_encoded
                    .extend_from_slice(&l1_batch_index_bytes)
                    .ok();

                let da_identifier_bytes = input.da_identifier.as_bytes();
                let mut da_identifier_encoded =
                    PushBytesBuf::with_capacity(da_identifier_bytes.len());
                da_identifier_encoded
                    .extend_from_slice(da_identifier_bytes)
                    .ok();

                let da_reference_bytes = input.blob_id.as_bytes();
                let mut da_reference_encoded =
                    PushBytesBuf::with_capacity(da_reference_bytes.len());
                da_reference_encoded
                    .extend_from_slice(da_reference_bytes)
                    .ok();

                final_script_result = basic_script
                    .push_slice(l1_batch_hash_encoded)
                    .push_slice(l1_batch_index_encoded)
                    .push_slice(da_identifier_encoded)
                    .push_slice(da_reference_encoded);
            }

            types::InscriptionMessage::ProofDAReference(input) => {
                let l1_batch_reveal_txid_bytes =
                    input.l1_batch_reveal_txid.as_raw_hash().as_byte_array();
                let mut l1_batch_reveal_txid_encoded =
                    PushBytesBuf::with_capacity(l1_batch_reveal_txid_bytes.len());
                l1_batch_reveal_txid_encoded
                    .extend_from_slice(l1_batch_reveal_txid_bytes)
                    .ok();

                let da_identifier_bytes = input.da_identifier.as_bytes();
                let mut da_identifier_encoded =
                    PushBytesBuf::with_capacity(da_identifier_bytes.len());
                da_identifier_encoded
                    .extend_from_slice(da_identifier_bytes)
                    .ok();

                let da_reference_bytes = input.blob_id.as_bytes();
                let mut da_reference_encoded =
                    PushBytesBuf::with_capacity(da_reference_bytes.len());
                da_reference_encoded
                    .extend_from_slice(da_reference_bytes)
                    .ok();

                final_script_result = basic_script
                    .push_slice(l1_batch_reveal_txid_encoded)
                    .push_slice(da_identifier_encoded)
                    .push_slice(da_reference_encoded);
            }

            types::InscriptionMessage::ValidatorAttestation(input) => {
                let reference_txid_bytes = input.reference_txid.as_raw_hash().as_byte_array();
                let mut reference_txid_encoded =
                    PushBytesBuf::with_capacity(reference_txid_bytes.len());
                reference_txid_encoded
                    .extend_from_slice(reference_txid_bytes)
                    .ok();

                match input.attestation {
                    types::Vote::Ok => {
                        final_script_result = basic_script
                            .push_slice(reference_txid_encoded)
                            .push_opcode(all::OP_PUSHNUM_1);
                    }
                    types::Vote::NotOk => {
                        final_script_result = basic_script
                            .push_slice(reference_txid_encoded)
                            .push_opcode(OP_0);
                    }
                }
            }

            types::InscriptionMessage::SystemBootstrapping(input) => {
                let start_block_height_bytes = input.start_block_height.to_be_bytes();
                let mut start_block_height_encoded =
                    PushBytesBuf::with_capacity(start_block_height_bytes.len());
                start_block_height_encoded
                    .extend_from_slice(&start_block_height_bytes)
                    .ok();

                let mut tapscript = basic_script.push_slice(start_block_height_encoded);

                for verifier_p2wpkh_address in input.verifier_p2wpkh_addresses {
                    let address_string = verifier_p2wpkh_address.to_string();
                    let verifier_p2wpkh_address_bytes = address_string.as_bytes();

                    let mut verifier_p2wpkh_addresses_encoded =
                        PushBytesBuf::with_capacity(verifier_p2wpkh_address_bytes.len());
                    verifier_p2wpkh_addresses_encoded
                        .extend_from_slice(verifier_p2wpkh_address_bytes)
                        .ok();

                    tapscript = tapscript.push_slice(verifier_p2wpkh_addresses_encoded);
                }

                let bridge_addr_string = input.bridge_p2wpkh_mpc_address.to_string();
                let bridge_p2wpkh_mpc_address_bytes = bridge_addr_string.as_bytes();
                let mut bridge_p2wpkh_mpc_address_encoded =
                    PushBytesBuf::with_capacity(bridge_p2wpkh_mpc_address_bytes.len());
                bridge_p2wpkh_mpc_address_encoded
                    .extend_from_slice(bridge_p2wpkh_mpc_address_bytes)
                    .ok();

                final_script_result = tapscript.push_slice(bridge_p2wpkh_mpc_address_encoded);
            }

            types::InscriptionMessage::ProposeSequencer(input) => {
                let addr_string = input.sequencer_new_p2wpkh_address.to_string();
                let sequencer_new_p2wpkh_address_bytes = addr_string.as_bytes();
                let mut sequencer_new_p2wpkh_address_encoded =
                    PushBytesBuf::with_capacity(sequencer_new_p2wpkh_address_bytes.len());
                sequencer_new_p2wpkh_address_encoded
                    .extend_from_slice(sequencer_new_p2wpkh_address_bytes)
                    .ok();

                final_script_result = basic_script.push_slice(sequencer_new_p2wpkh_address_encoded);
            }

            types::InscriptionMessage::L1ToL2Message(input) => {
                let receiver_l2_address_bytes = input.receiver_l2_address.as_bytes();
                let mut receiver_l2_address_encoded =
                    PushBytesBuf::with_capacity(receiver_l2_address_bytes.len());
                receiver_l2_address_encoded
                    .extend_from_slice(receiver_l2_address_bytes)
                    .ok();

                let l2_contract_address_bytes = input.l2_contract_address.as_bytes();
                let mut l2_contract_address_encoded =
                    PushBytesBuf::with_capacity(l2_contract_address_bytes.len());
                l2_contract_address_encoded
                    .extend_from_slice(l2_contract_address_bytes)
                    .ok();

                let mut call_data_encoded = PushBytesBuf::with_capacity(input.call_data.len());
                call_data_encoded.extend_from_slice(&input.call_data).ok();

                final_script_result = basic_script
                    .push_slice(receiver_l2_address_encoded)
                    .push_slice(l2_contract_address_encoded)
                    .push_slice(call_data_encoded);
            }
        }

        let final_script_result = final_script_result.push_opcode(all::OP_ENDIF).into_script();

        let script_size = final_script_result.len();

        Ok((final_script_result, script_size))
    }
}
