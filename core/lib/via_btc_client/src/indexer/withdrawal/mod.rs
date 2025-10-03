use std::convert::TryFrom;

use bitcoin::{Address, Amount};

const WITHDRAWAL_BYTE_SIZE: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum WithdrawalVersion {
    Version0 = 0,
}

impl TryFrom<u8> for WithdrawalVersion {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(WithdrawalVersion::Version0),
            _ => anyhow::bail!("Invalid withdrawal version"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct L1Withdrawal {
    pub l2_meta: L2WithdrawalMeta,
    pub receiver: Address,
    pub value: Amount,
}

#[derive(Debug, Clone, PartialEq)]
pub struct L2WithdrawalMeta {
    /// First 8 bytes of the L2 hash, stored as hex (16 chars)
    pub l2_id: String,
    /// The next 2 bytes contains the index of the log where the withdrawal was executed
    pub l2_tx_event_index: u16,
}

impl L2WithdrawalMeta {
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() != WITHDRAWAL_BYTE_SIZE {
            anyhow::bail!(
                "Invalid byte size to decode withdrawal meta, expected {} received {}",
                WITHDRAWAL_BYTE_SIZE,
                bytes.len()
            )
        }

        let l2_id = hex::encode(&bytes[..8]);

        let l2_tx_event_index_bytes: [u8; 2] = bytes[8..].try_into().unwrap();
        let l2_tx_event_index = u16::from_be_bytes(l2_tx_event_index_bytes);

        Ok(Self {
            l2_id,
            l2_tx_event_index,
        })
    }

    pub fn to_bytes(&self) -> anyhow::Result<[u8; WITHDRAWAL_BYTE_SIZE]> {
        let mut buf = [0u8; WITHDRAWAL_BYTE_SIZE];

        // convert hex string back into 8 raw bytes
        let id_bytes = hex::decode(&self.l2_id)?;
        if id_bytes.len() != 8 {
            anyhow::bail!(
                "l2_id must decode into exactly 8 bytes, got {}",
                id_bytes.len()
            );
        }
        buf[..8].copy_from_slice(&id_bytes);

        // last 2 bytes
        let index_bytes = self.l2_tx_event_index.to_be_bytes();
        buf[8..].copy_from_slice(&index_bytes);

        Ok(buf)
    }
}

pub fn parse_withdrawal(
    version: WithdrawalVersion,
    bytes: &[u8],
) -> anyhow::Result<L2WithdrawalMeta> {
    match version {
        WithdrawalVersion::Version0 => L2WithdrawalMeta::from_bytes(bytes),
    }
}

pub fn parse_withdrawals(
    version: WithdrawalVersion,
    bytes: &[u8],
) -> anyhow::Result<Vec<L2WithdrawalMeta>> {
    match version {
        WithdrawalVersion::Version0 => {
            let record_size = 10;
            if bytes.len() % record_size != 0 {
                anyhow::bail!(
                    "buffer length {} is not a multiple of {}",
                    bytes.len(),
                    record_size
                );
            }

            let mut withdrawals = Vec::new();
            for chunk in bytes.chunks(record_size) {
                withdrawals.push(L2WithdrawalMeta::from_bytes(chunk)?);
            }
            Ok(withdrawals)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_withdrawal_meta() {
        let tx_hash_hex = "a3f207a872cc5a8618cb5ff7a59d53f2f9302a38f5b7b65e7112c6a0f56edc3a";
        let tx_hash_bytes: Vec<u8> = hex::decode(tx_hash_hex).unwrap();

        // First 8 bytes → hex string
        let l2_id = hex::encode(&tx_hash_bytes[..8]);

        let meta = L2WithdrawalMeta {
            l2_id,
            l2_tx_event_index: 0x1234,
        };

        let encoded = meta.to_bytes().unwrap();
        let decoded = L2WithdrawalMeta::from_bytes(&encoded).unwrap();

        assert_eq!(meta, decoded);
    }
}
