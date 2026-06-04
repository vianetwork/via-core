use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub enum ViaNodeRole {
    Verifier,
    Coordinator,
    VerifierAndProcessor,
}
