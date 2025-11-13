use zksync_types::ProtocolVersionId;

use crate::{
    version_27::{types::ProveBatches as ProveBatchesV27, verify_proof as verify_proof_v27},
    version_28::{types::ProveBatches as ProveBatchesV28, verify_proof as verify_proof_v28},
};

pub mod version_27;
pub mod version_28;

#[derive(Clone)]
pub enum ProveBatchData {
    V27(ProveBatchesV27),
    V28(ProveBatchesV28),
}

/// Decodes the proof data into the appropriate version. It's possible that the data serialization format changes even within the same version.
pub fn decode_prove_batch_data(
    protocol_version_id: ProtocolVersionId,
    proof_data: &[u8],
) -> anyhow::Result<ProveBatchData> {
    if protocol_version_id <= ProtocolVersionId::Version27 {
        if let Ok(prove_batch) = bincode::deserialize::<ProveBatchesV27>(proof_data) {
            tracing::info!("Decode proof data with V27");
            return Ok(ProveBatchData::V27(prove_batch));
        }
        tracing::warn!("Failed to decode proof data as V27");
    }

    if let Ok(prove_batch) = bincode::deserialize::<ProveBatchesV28>(proof_data) {
        tracing::info!("Decode proof data with V28");
        return Ok(ProveBatchData::V28(prove_batch));
    }

    // If both fail, return an error
    anyhow::bail!("Failed to decode proof data as either V27 or V28");
}

pub async fn verify_proof(prover_batch_data: ProveBatchData) -> anyhow::Result<bool> {
    match prover_batch_data {
        ProveBatchData::V27(data) => verify_proof_v27(data).await,
        ProveBatchData::V28(data) => verify_proof_v28(data).await,
    }
}
