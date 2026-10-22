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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_and_child_manifest_golden_bytes() {
        // Hand-assembled u64 LE chunk/data lengths, followed by data, using fixed-int defaults:
        // https://github.com/servo/bincode/blob/c44b5e364e7084cdbabf9f94b63a3c7f32b8fb68/src/lib.rs
        // Manifest entries are hand-assembled: u32 BE ID length, then the ID bytes.
        let ids = vec![
            "0102030405060708000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                .to_owned(),
            "1112131415161718202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f"
                .to_owned(),
        ];
        let manifest = concat!(
            "000000280102030405060708000102030405060708090a0b0c0d0e0f101112131415",
            "161718191a1b1c1d1e1f000000281112131415161718202122232425262728292a2b",
            "2c2d2e2f303132333435363738393a3b3c3d3e3f",
        );
        let root = concat!(
            "020000000000000058000000000000000000002801020304050607080001020304",
            "05060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f000000281112",
            "131415161718202122232425262728292a2b2c2d2e2f303132333435363738393a3b",
            "3c3d3e3f",
        );
        let manifest_bytes = hex::decode(manifest).unwrap();
        assert_eq!(serialize_blob_ids(&ids).unwrap(), manifest_bytes);
        assert_eq!(deserialize_blob_ids(&manifest_bytes).unwrap(), ids);
        for (blob, encoded) in [
            (
                ViaDaBlob::new(1, vec![0x00, 0x01, 0x02, 0xff]),
                "01000000000000000400000000000000000102ff",
            ),
            (ViaDaBlob::new(2, manifest_bytes), root),
        ] {
            let bytes = hex::decode(encoded).unwrap();
            assert_eq!(blob.to_bytes(), bytes);
            let decoded = ViaDaBlob::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.chunks, blob.chunks);
            assert_eq!(decoded.data, blob.data);
        }
    }

    #[test]
    fn child_manifest_empty_and_invalid_hex() {
        assert!(serialize_blob_ids(&[]).unwrap().is_empty());
        assert!(deserialize_blob_ids(&[]).unwrap().is_empty());
        assert!(serialize_blob_ids(&["0g".to_owned()]).is_err());
        assert!(serialize_blob_ids(&["0".to_owned()]).is_err());
    }
}
