use zksync_types::{
    btc_block::ViaBtcL1BlockDetails, btc_inscription_operations::ViaBtcInscriptionRequestType,
};

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ViaAggregatedOperation {
    CommitL1BatchOnchain(Vec<ViaBtcL1BlockDetails>),
    CommitProofOnchain(Vec<ViaBtcL1BlockDetails>),
}

impl ViaAggregatedOperation {
    pub fn get_l1_batches_detail(&self) -> &Vec<ViaBtcL1BlockDetails> {
        match self {
            Self::CommitL1BatchOnchain(l1_batch) => l1_batch,
            Self::CommitProofOnchain(l1_batch) => l1_batch,
        }
    }

    pub fn get_inscription_request_type(&self) -> ViaBtcInscriptionRequestType {
        match self {
            Self::CommitL1BatchOnchain(..) => ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
            Self::CommitProofOnchain(..) => ViaBtcInscriptionRequestType::CommitProofOnchain,
        }
    }
}
