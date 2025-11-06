use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViaDaBlob {
    pub chunks: usize,
    pub data: Vec<u8>,
}

impl ViaDaBlob {
    pub fn new(chunks: usize, data: Vec<u8>) -> Self {
        Self { chunks, data }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).expect("Failed to serialize ViaDaBlob")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        bincode::deserialize(bytes).ok()
    }
}

pub fn serialize_blob_ids(hex_vec: &[String]) -> anyhow::Result<Vec<u8>> {
    let mut result = Vec::new();

    for hex_str in hex_vec {
        let bytes = hex::decode(hex_str)?;
        let len = bytes.len() as u32;

        // Write 4-byte length prefix (big-endian)
        result.extend_from_slice(&len.to_be_bytes());
        result.extend_from_slice(&bytes);
    }

    Ok(result)
}

pub fn deserialize_blob_ids(data: &[u8]) -> anyhow::Result<Vec<String>> {
    let mut pos = 0;
    let mut result = Vec::new();

    while pos < data.len() {
        // Read the 4-byte length prefix
        let len_bytes: [u8; 4] = data[pos..pos + 4].try_into()?;
        let len = u32::from_be_bytes(len_bytes) as usize;
        pos += 4;

        // Extract the chunk
        let chunk = &data[pos..pos + len];
        pos += len;

        result.push(hex::encode(chunk));
    }

    Ok(result)
}
