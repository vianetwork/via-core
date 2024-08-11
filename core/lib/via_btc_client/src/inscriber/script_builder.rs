// Witness Structure for each message type
// in our case da_identifier is b"celestia"

// L1BatchDAReference
// |----------------------------------------------------------|
// |      Schnorr Signature                                   |
// |      Encoded Sequencer Public Key                        |
// |      OP_CHECKSIG                                         |
// |      OP_FALSE                                            |
// |      OP_IF                                               |
// |      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')" |
// |      OP_PUSHBYTES_32  b"Str('L1BatchDAReference')"       |
// |      OP_PUSHBYTES_32  b"l1_batch_hash"                   |
// |      OP_PUSHBYTES_32  b"l1_batch_index"                  |
// |      OP_PUSHBYTES_32  b"celestia"                        |
// |      OP_PUSHBYTES_2   b"da_reference"                    |
// |      OP_ENDIF                                            |
// |----------------------------------------------------------|

// ProofDAReferenceMessage
// |----------------------------------------------------------|
// |      Schnorr Signature                                   |
// |      Encoded Sequencer Public Key                        |
// |      OP_CHECKSIG                                         |
// |      OP_FALSE                                            |
// |      OP_IF                                               |
// |      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')" |
// |      OP_PUSHBYTES_32  b"Str('ProofDAReferenceMessage')"  |
// |      OP_PUSHBYTES_32  b"l1_batch_reveal_txid"            |
// |      OP_PUSHBYTES_32  b"celestia"                        |
// |      OP_PUSHBYTES_2   b"da_reference"                    |
// |      OP_ENDIF                                            |
// |----------------------------------------------------------|

// OP_1 means ok or valid
// OP_0 means not ok ok or invalid
// reference_txid could be the proof_reveal_txid or other administrative inscription txid

// ValidatorAttestationMessage
// |-------------------------------------------------------------|
// |      Schnorr Signature                                      |
// |      Encoded Verifier Public Key                            |
// |      OP_CHECKSIG                                            |
// |      OP_FALSE                                               |
// |      OP_IF                                                  |
// |      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')"    |
// |      OP_PUSHBYTES_32  b"Str('ValidatorAttestationMessage')" |
// |      OP_PUSHBYTES_32  b"reference_txid"                     |
// |      OP_PUSHBYTES_1   b"OP_1" /  b"OP_0"                    |
// |      OP_ENDIF                                               |
// |-------------------------------------------------------------|

// System Bootstrapping Message (txid should be part of genesis state in verifier network)
// |-------------------------------------------------------------|
// |      Schnorr Signature                                      |
// |      Encoded Verifier Public Key                            |
// |      OP_CHECKSIG                                            |
// |      OP_FALSE                                               |
// |      OP_IF                                                  |
// |      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')"    |
// |      OP_PUSHBYTES_32  b"Str('SystemBootstrappingMessage')"  |
// |      OP_PUSHBYTES_32  b"start_block_height"                 |
// |      OP_PUSHBYTES_32  b"verifier_1_p2wpkh_address"          |
// |      OP_PUSHBYTES_32  b"verifier_2_p2wpkh_address"          |
// |      OP_PUSHBYTES_32  b"verifier_3_p2wpkh_address"          |
// |      OP_PUSHBYTES_32  b"verifier_4_p2wpkh_address"          |
// |      OP_PUSHBYTES_32  b"verifier_5_p2wpkh_address"          |
// |      OP_PUSHBYTES_32  b"verifier_6_p2wpkh_address"          |
// |      OP_PUSHBYTES_32  b"verifier_7_p2wpkh_address"          |
// |      OP_PUSHBYTES_32  b"bridge_p2wpkh_mpc_address"          |
// |      OP_ENDIF                                               |
// |-------------------------------------------------------------|

// Propose Sequencer Message
// verifier should sent attestation to network to validate this message
// |-------------------------------------------------------------|
// |      Schnorr Signature                                      |
// |      Encoded Verifier Public Key                            |
// |      OP_CHECKSIG                                            |
// |      OP_FALSE                                               |
// |      OP_IF                                                  |
// |      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')"    |
// |      OP_PUSHBYTES_32  b"Str('ProposeSequencerMessage')"     |
// |      OP_PUSHBYTES_32  b"proposer_p2wpkh_address"            |
// |      OP_ENDIF                                               |
// |-------------------------------------------------------------|

// L1ToL2Message
// |-------------------------------------------------------------|
// |      Schnorr Signature                                      |
// |      Encoded USER/Admin Public Key                          |
// |      OP_CHECKSIG                                            |
// |      OP_FALSE                                               |
// |      OP_IF                                                  |
// |      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')"    |
// |      OP_PUSHBYTES_32  b"Str('L1ToL2Message')"               |
// |      OP_PUSHBYTES_32  b"receiver_l2_address"                |
// |      OP_PUSHBYTES_32  b"l2_contract_address"                |
// |      OP_PUSHBYTES_32  b"call_data"                          |
// |      OP_ENDIF                                               |
// |-------------------------------------------------------------|
//  !!! for bridging the l2_contract_address and call_data is empty (0x00) !!!
//  !!! and the amount is equal to the amount of btc user sends to bridge address in the same reveal tx !!!
//  !!! if the contract address and call_data was provided the amount get used as fee and remaining amount get sent to l2 receiver address !!!
//  !!! in future we can implement kinda enforcement withdrawal with using l1->l2 message (reference in notion) !!!
//  !!! also we should support op_return only for bridging in future of the inscription indexer !!!

use crate::inscriber::types;
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
        inscription_message: types::InscriberInput,
        secp: &Secp256k1<C>,
        internal_key: UntweakedPublicKey,
        network: Network,
    ) -> Result<Self> {
        let serelized_pubkey = internal_key.serialize();
        let mut encoded_pubkey = PushBytesBuf::with_capacity(serelized_pubkey.len());
        encoded_pubkey.extend_from_slice(&serelized_pubkey).ok();

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
        message: types::InscriberInput,
    ) -> Result<(ScriptBuf, usize)> {
        let final_script_result: ScriptBuilder;

        match message {
            types::InscriberInput::L1BatchDAReference {
                l1_batch_hash,
                l1_batch_index,
                da_reference,
            } => {
                let l1_batch_hash_bytes = l1_batch_hash.as_bytes();
                let mut l1_batch_hash_encoded =
                    PushBytesBuf::with_capacity(l1_batch_hash_bytes.len());
                l1_batch_hash_encoded
                    .extend_from_slice(l1_batch_hash_bytes)
                    .ok();

                let l1_batch_index_bytes = l1_batch_index.to_be_bytes();
                let mut l1_batch_index_encoded =
                    PushBytesBuf::with_capacity(l1_batch_index_bytes.len());
                l1_batch_index_encoded
                    .extend_from_slice(&l1_batch_index_bytes)
                    .ok();

                let da_reference_bytes = da_reference.blob_id.as_bytes();
                let mut da_reference_encoded =
                    PushBytesBuf::with_capacity(da_reference_bytes.len());
                da_reference_encoded
                    .extend_from_slice(da_reference_bytes)
                    .ok();

                final_script_result = basic_script
                    .push_slice(l1_batch_hash_encoded)
                    .push_slice(l1_batch_index_encoded)
                    .push_slice(da_reference_encoded);
            }

            types::InscriberInput::ProofDAReference {
                l1_batch_reveal_txid,
                da_reference,
            } => {
                let l1_batch_reveal_txid_bytes = l1_batch_reveal_txid.as_raw_hash().as_byte_array();
                let mut l1_batch_reveal_txid_encoded =
                    PushBytesBuf::with_capacity(l1_batch_reveal_txid_bytes.len());
                l1_batch_reveal_txid_encoded
                    .extend_from_slice(l1_batch_reveal_txid_bytes)
                    .ok();

                let da_reference_bytes = da_reference.blob_id.as_bytes();
                let mut da_reference_encoded =
                    PushBytesBuf::with_capacity(da_reference_bytes.len());
                da_reference_encoded
                    .extend_from_slice(da_reference_bytes)
                    .ok();

                final_script_result = basic_script
                    .push_slice(l1_batch_reveal_txid_encoded)
                    .push_slice(da_reference_encoded);
            }

            types::InscriberInput::ValidatorAttestation {
                reference_txid,
                vote,
            } => {
                let reference_txid_bytes = reference_txid.as_raw_hash().as_byte_array();
                let mut reference_txid_encoded =
                    PushBytesBuf::with_capacity(reference_txid_bytes.len());
                reference_txid_encoded
                    .extend_from_slice(reference_txid_bytes)
                    .ok();

                match vote {
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

            types::InscriberInput::SystemBootstrapping {
                start_block_height,
                verifier_p2wpkh_addresses,
                bridge_p2wpkh_mpc_address,
            } => {
                let start_block_height_bytes = start_block_height.to_be_bytes();
                let mut start_block_height_encoded =
                    PushBytesBuf::with_capacity(start_block_height_bytes.len());
                start_block_height_encoded
                    .extend_from_slice(&start_block_height_bytes)
                    .ok();

                let mut tapscript = basic_script.push_slice(start_block_height_encoded);

                for verifier_p2wpkh_address in verifier_p2wpkh_addresses {
                    let address_string = verifier_p2wpkh_address.to_string();
                    let verifier_p2wpkh_address_bytes = address_string.as_bytes();

                    let mut verifier_p2wpkh_addresses_encoded =
                        PushBytesBuf::with_capacity(verifier_p2wpkh_address_bytes.len());
                    verifier_p2wpkh_addresses_encoded
                        .extend_from_slice(verifier_p2wpkh_address_bytes)
                        .ok();

                    tapscript = tapscript.push_slice(verifier_p2wpkh_addresses_encoded);
                }

                let bridge_addr_string = bridge_p2wpkh_mpc_address.to_string();
                let bridge_p2wpkh_mpc_address_bytes = bridge_addr_string.as_bytes();
                let mut bridge_p2wpkh_mpc_address_encoded =
                    PushBytesBuf::with_capacity(bridge_p2wpkh_mpc_address_bytes.len());
                bridge_p2wpkh_mpc_address_encoded
                    .extend_from_slice(bridge_p2wpkh_mpc_address_bytes)
                    .ok();

                final_script_result = tapscript.push_slice(bridge_p2wpkh_mpc_address_encoded);
            }

            types::InscriberInput::ProposeSequencer {
                sequencer_new_p2wpkh_address,
            } => {
                let addr_string = sequencer_new_p2wpkh_address.to_string();
                let sequencer_new_p2wpkh_address_bytes = addr_string.as_bytes();
                let mut sequencer_new_p2wpkh_address_encoded =
                    PushBytesBuf::with_capacity(sequencer_new_p2wpkh_address_bytes.len());
                sequencer_new_p2wpkh_address_encoded
                    .extend_from_slice(sequencer_new_p2wpkh_address_bytes)
                    .ok();

                final_script_result = basic_script.push_slice(sequencer_new_p2wpkh_address_encoded);
            }

            types::InscriberInput::L1ToL2Message {
                receiver_l2_address,
                l2_contract_address,
                call_data,
            } => {
                let receiver_l2_address_bytes = receiver_l2_address.as_bytes();
                let mut receiver_l2_address_encoded =
                    PushBytesBuf::with_capacity(receiver_l2_address_bytes.len());
                receiver_l2_address_encoded
                    .extend_from_slice(receiver_l2_address_bytes)
                    .ok();

                let l2_contract_address_bytes = l2_contract_address.as_bytes();
                let mut l2_contract_address_encoded =
                    PushBytesBuf::with_capacity(l2_contract_address_bytes.len());
                l2_contract_address_encoded
                    .extend_from_slice(l2_contract_address_bytes)
                    .ok();

                let mut call_data_encoded = PushBytesBuf::with_capacity(call_data.len());
                call_data_encoded.extend_from_slice(&call_data).ok();

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
