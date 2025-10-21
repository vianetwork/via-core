use zksync_types::ProtocolVersionId;

use crate::{
    version_27::verify_proof as verify_proof_v27, version_28::verify_proof as verify_proof_v28,
};

pub mod version_27;
pub mod version_28;

pub async fn verify_proof(
    protocol_version_id: ProtocolVersionId,
    proof_data: &[u8],
) -> anyhow::Result<bool> {
    match protocol_version_id {
        ProtocolVersionId::Version26 | ProtocolVersionId::Version27 => {
            return verify_proof_v27(proof_data).await
        }
        ProtocolVersionId::Version28 => return verify_proof_v28(proof_data).await,
        _ => {
            anyhow::bail!("Verification not supported")
        }
    }
}
