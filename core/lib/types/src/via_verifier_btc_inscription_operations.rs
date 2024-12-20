use std::{fmt, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViaVerifierBtcInscriptionRequestType {
    VoteOnchain,
}

impl ViaVerifierBtcInscriptionRequestType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VoteOnchain => "VoteOnchain",
        }
    }
}

impl fmt::Display for ViaVerifierBtcInscriptionRequestType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for ViaVerifierBtcInscriptionRequestType {
    fn from(s: String) -> Self {
        match s.as_str() {
            "VoteOnchain" => ViaVerifierBtcInscriptionRequestType::VoteOnchain,
            _ => panic!(
                "Unexpected value for ViaVerifierBtcInscriptionRequestType: {}",
                s
            ),
        }
    }
}

impl FromStr for ViaVerifierBtcInscriptionRequestType {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "VoteOnchain" => Ok(Self::VoteOnchain),
            _ => Err(
                "Incorrect aggregated action type; expected one of `VoteOnchain`, `CommitProofOnchain`",
            ),
        }
    }
}
