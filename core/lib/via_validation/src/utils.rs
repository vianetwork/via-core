use std::{env, path::PathBuf};

use crate::errors::VerificationError;

/// Checks if the verification key exists for a given protocol version.
pub async fn check_verification_key(protocol_version: u16) -> Result<(), VerificationError> {
    let file_path = format!(
        "keys/protocol_version/{}/scheduler_key.json",
        protocol_version
    );
    let base_dir =
        env::var("CARGO_MANIFEST_DIR").map_err(|e| VerificationError::Other(e.to_string()))?;
    let base_path = PathBuf::from(base_dir);
    let file = base_path.join(file_path);
    let file_exists = file.exists();

    if !file_exists {
        Err(VerificationError::Other(format!(
            "Verification key for protocol version {} is missing. Please add it to the keys folder.",
            protocol_version
        )))
    } else {
        Ok(())
    }
}

pub(crate) fn to_fixed_bytes(ins: &[u8]) -> [u8; 32] {
    let mut result = [0u8; 32];
    result.copy_from_slice(ins);

    result
}
