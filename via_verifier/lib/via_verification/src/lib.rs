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

pub fn decode_prove_batch_data(
    protocol_version_id: ProtocolVersionId,
    proof_data: &[u8],
) -> anyhow::Result<ProveBatchData> {
    match protocol_version_id {
        ProtocolVersionId::Version26 | ProtocolVersionId::Version27 => {
            let prove_batch: ProveBatchesV27 = bincode::deserialize(proof_data)?;
            Ok(ProveBatchData::V27(prove_batch))
        }
        ProtocolVersionId::Version28 => {
            let prove_batch: ProveBatchesV28 = bincode::deserialize(proof_data)?;
            Ok(ProveBatchData::V28(prove_batch))
        }
        _ => anyhow::bail!("Unsupported prove batch version: {}", protocol_version_id),
    }
}

pub async fn verify_proof(prover_batch_data: ProveBatchData) -> anyhow::Result<bool> {
    match prover_batch_data {
        ProveBatchData::V27(data) => verify_proof_v27(data).await,
        ProveBatchData::V28(data) => verify_proof_v28(data).await,
    }
}
