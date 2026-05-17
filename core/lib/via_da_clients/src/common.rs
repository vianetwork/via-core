use anyhow::anyhow;
use celestia_types::Commitment;
use zksync_da_client::types;

pub const VIA_NAME_SPACE_BYTES: [u8; 8] = [b'V', b'I', b'A', 0, 0, 0, 0, 0];

pub(crate) fn parse_blob_id(blob_id: &str) -> anyhow::Result<(Commitment, u64)> {
    // [8]byte block height ++ [32]byte commitment
    let blob_id_bytes = hex::decode(blob_id).map_err(|error| types::DAError {
        error: error.into(),
        is_retriable: false,
    })?;

    let block_height =
        u64::from_be_bytes(blob_id_bytes[..8].try_into().map_err(|_| types::DAError {
            error: anyhow!("Failed to convert block height"),
            is_retriable: false,
        })?);

    let commitment_data: [u8; 32] =
        blob_id_bytes[8..40]
            .try_into()
            .map_err(|_| types::DAError {
                error: anyhow!("Failed to convert commitment"),
                is_retriable: false,
            })?;

    let commitment = Commitment(commitment_data);

    Ok((commitment, block_height))
}
