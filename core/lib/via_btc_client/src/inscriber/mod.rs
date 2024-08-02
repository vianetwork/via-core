use crate::traits::BitcoinRpc;
use crate::traits::BitcoinSigner;
use anyhow::{Context, Result};
use bitcoin::script::{Builder as ScriptBuilder, PushBytesBuf};
use bitcoin::{
    transaction, Address, Amount, CompressedPublicKey, Network, OutPoint, PrivateKey, ScriptBuf,
    Sequence, TapLeafHash, Transaction, TxIn, TxOut, Txid, WPubkeyHash, Witness,
};
use bitcoin::opcodes::{all, OP_FALSE};


// @TODO - Implement input validation for all the messages
pub struct L1BatchDAReference {
    pub l1_batch_hash: Vec<u8>,
    pub l1_batch_index: Vec<u8>,
    pub da_identifier: Vec<u8>,
    pub da_reference: Vec<u8>,
}
pub struct ProofDAReferenceMessage {
    pub l1_batch_reveal_txid: Vec<u8>,
    pub da_identifier: Vec<u8>,
    pub da_reference: Vec<u8>,
}

pub struct ValidatorAttestationMessage {
    pub proof_reveal_txid: Vec<u8>,
    pub is_valid: bool,
}

pub struct L1ToL2Message {
    pub destination_address: Vec<u8>,
    pub call_data: Vec<u8>,
}

pub enum Message {
    L1BatchDAReference(L1BatchDAReference),
    ProofDAReferenceMessage(ProofDAReferenceMessage),
    ValidatorAttestationMessage(ValidatorAttestationMessage),
    L1ToL2Message(L1ToL2Message),
}

pub struct Inscriber<'a> {
    btc_signer: &'a dyn BitcoinSigner<'a>,
    btc_client: &'a dyn BitcoinRpc,
}

// @TODO - Implement methods and structs to enable chainable methods 

impl<'a> Inscriber<'a> {
    pub fn new(
        btc_signer: &'a dyn BitcoinSigner<'a>,
        btc_client: &'a dyn BitcoinRpc,
    ) -> Result<Self> {
        Ok(Inscriber {
            btc_signer,
            btc_client,
        })
    }

    pub async fn inscribe(&self, inscription_message: Message) -> Result<()> {
        match inscription_message {
            Message::L1BatchDAReference(msg) => {
                // Handle L1BatchDAReference
            }
            Message::ProofDAReferenceMessage(msg) => {
                // Handle ProofDAReferenceMessage
            }
            Message::ValidatorAttestationMessage(msg) => {
                // Handle ValidatorAttestationMessage
            }
            Message::L1ToL2Message(msg) => {
                // Handle L1ToL2Message
            }
            _ => {
                // Handle mismatched types
                return Err(anyhow::anyhow!("Mismatched inscription type and message"));
            }
        }


        Ok(())
    }

    fn construct_base_script(&self) -> Result<ScriptBuilder> {
        todo!();

        // Modify the signer and add needed methods and data to the struct
        let encoded_pubkey = self
            .btc_signer
            .get_public_key()
            .context("Failed to get public key")?;

        let mut script = ScriptBuilder::new()
            .push_slice(encoded_pubkey.as_push_bytes())
            .push_opcode(all::OP_CHECKSIG)
            .push_opcode(OP_FALSE)
            .push_opcode(all::OP_IF);
        
        Ok(script)
    }

}


// Example: final revel witness for each message type
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

