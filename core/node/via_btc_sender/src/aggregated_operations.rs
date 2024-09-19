use via_btc_client::types::InscriptionMessage;
use zksync_types::{
    btc_block::ViaBtcL1BlockDetails, btc_inscription_operations::ViaBtcInscriptionRequestType,
};

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ViaAggregatedOperation {
    CommitL1BatchOnchain(Vec<ViaBtcL1BlockDetails>, Vec<InscriptionMessage>),
    CommitProofOnchain(Vec<ViaBtcL1BlockDetails>, Vec<InscriptionMessage>),
}

impl ViaAggregatedOperation {
    pub fn get_action_type(&self) -> ViaBtcInscriptionRequestType {
        match self {
            Self::CommitL1BatchOnchain(..) => ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
            Self::CommitProofOnchain(..) => ViaBtcInscriptionRequestType::CommitProofOnchain,
        }
    }

    pub fn get_l1_batches_detail(&self) -> Vec<ViaBtcL1BlockDetails> {
        match self {
            Self::CommitL1BatchOnchain(l1_batch, _) => l1_batch.clone(),
            Self::CommitProofOnchain(l1_batch, _) => l1_batch.clone(),
        }
    }

    pub fn get_inscription_messages(&self) -> Vec<InscriptionMessage> {
        match self {
            Self::CommitL1BatchOnchain(_, message) => message.clone(),
            Self::CommitProofOnchain(_, message) => message.clone(),
        }
    }
}
