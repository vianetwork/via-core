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

#[cfg(test)]
mod tests {
    use circuit_definitions::snark_wrapper::franklin_crypto::bellman::{pairing::bn256::Fr, PrimeField};
    use primitive_types::H256;
    use sha3::{Digest, Keccak256};

    use super::{version_27, version_28};

    #[test]
    fn public_input_golden_vectors_pin_both_versions_and_order() {
        // Digests computed separately with Go's legacy Keccak, calibrated against TestKeccak:
        // https://github.com/golang/crypto/blob/9d2ee975ef9fe627bf0a6f01c1f69e8ef1d4f05d/sha3/sha3_test.go
        // Decimal fields independently interpret the first 28 digest bytes as big-endian:
        // https://github.com/matter-labs/era-contracts/blob/96d41f23cbef2b570bd38b9d482cd7f54c81766a/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol
        let vectors = [
            (
                concat!(
                    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                    "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
                ),
                "002030bde3d4cf89919649775cd71875c4d0ab1708a380e03fefc3a28aa24831",
                "13242361443329087246156420440114732504830183209887327490336080802",
            ),
            (
                concat!(
                    "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
                    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                ),
                "ea80d4c77991d38cc023452c5f868d11d92d33eb522bd0eb7c05eea8bc264469",
                "24696074319266167286684706410972713011884085052862940949863205301928",
            ),
        ];
        let generators: [fn(&H256, &H256) -> Vec<Fr>; 2] = [
            version_27::public_inputs::generate_inputs,
            version_28::public_inputs::generate_inputs,
        ];
        for (preimage, digest, field_decimal) in vectors {
            let preimage = hex::decode(preimage).unwrap();
            assert_eq!(hex::encode(Keccak256::digest(&preimage)), digest);
            let previous = H256::from_slice(&preimage[..32]);
            let current = H256::from_slice(&preimage[32..]);
            let expected = vec![Fr::from_str(field_decimal).unwrap()];
            for generate in generators {
                assert_eq!(generate(&previous, &current), expected);
            }
        }
    }
}
