use std::{fmt, str::FromStr};

use crate::aggregated_operations::AggregatedActionType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViaBtcInscriptionRequestType {
    CommitL1BatchOnchain,
    CommitProofOnchain,
}

impl ViaBtcInscriptionRequestType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommitL1BatchOnchain => "CommitL1BatchOnchain",
            Self::CommitProofOnchain => "CommitProofOnchain",
        }
    }
}

impl fmt::Display for ViaBtcInscriptionRequestType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for ViaBtcInscriptionRequestType {
    fn from(s: String) -> Self {
        match s.as_str() {
            "CommitL1BatchOnchain" => ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
            "CommitProofOnchain" => ViaBtcInscriptionRequestType::CommitProofOnchain,
            _ => panic!("Unexpected value for ViaBtcInscriptionRequestType: {}", s),
        }
    }
}

impl From<ViaBtcInscriptionRequestType> for AggregatedActionType {
    fn from(tx_type: ViaBtcInscriptionRequestType) -> Self {
        match tx_type {
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain => AggregatedActionType::Commit,
            ViaBtcInscriptionRequestType::CommitProofOnchain => {
                AggregatedActionType::PublishProofOnchain
            }
        }
    }
}

impl FromStr for ViaBtcInscriptionRequestType {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "CommitL1BatchOnchain" => Ok(Self::CommitL1BatchOnchain),
            "CommitProofOnchain" => Ok(Self::CommitProofOnchain),
            _ => Err(
                "Incorrect aggregated action type; expected one of `CommitL1BatchOnchain`, `CommitProofOnchain`",
            ),
        }
    }
}
