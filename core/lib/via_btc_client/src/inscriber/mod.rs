use crate::traits::BitcoinRpc;
use crate::traits::BitcoinSigner;
use anyhow::{Context, Result};


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

    fn construct_base_script(&self) -> Result<()> {
        // Construct base script
        Ok(())
    }

}
