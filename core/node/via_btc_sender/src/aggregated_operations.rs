use via_btc_client::types::InscriptionMessage;
use zksync_types::{
    btc_inscription_operations::ViaBtcInscriptionRequestType, commitment::L1BatchWithMetadata,
};

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ViaAggregatedOperation {
    CommitL1BatchOnchain(L1BatchWithMetadata, InscriptionMessage),
    CommitProofOnchain(L1BatchWithMetadata, InscriptionMessage),
}

impl ViaAggregatedOperation {
    pub fn get_action_type(&self) -> ViaBtcInscriptionRequestType {
        match self {
            Self::CommitL1BatchOnchain(..) => ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
            Self::CommitProofOnchain(..) => ViaBtcInscriptionRequestType::CommitProofOnchain,
        }
    }

    pub fn get_l1_batch_metadata(&self) -> L1BatchWithMetadata {
        match self {
            Self::CommitL1BatchOnchain(l1_batch, _) => l1_batch.clone(),
            Self::CommitProofOnchain(l1_batch, _) => l1_batch.clone(),
        }
    }

    pub fn get_inscription_message(&self) -> InscriptionMessage {
        match self {
            Self::CommitL1BatchOnchain(_, message) => message.clone(),
            Self::CommitProofOnchain(_, message) => message.clone(),
        }
    }
}
