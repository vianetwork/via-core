use std::io::Read;

use anyhow::Context;
use bitcoin::{address::NetworkUnchecked, Address as BitcoinAddress};
use byteorder::{BigEndian, ReadBytesExt};
use zksync_types::{Address, H160, H256, U256};
use zksync_utils::{u256_to_bytes_be, u256_to_h256};

/// The function selector used in L2 to compute the message.
pub const WITHDRAW_FUNC_SIG: &str = "finalizeEthWithdrawal(uint256,uint256,uint16,bytes,bytes32[])";

/// The L2 system bridge address.
pub const L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR: &str = "000000000000000000000000000000000000800a";

#[derive(Clone, Debug, Default)]
#[allow(unused)]
pub struct L2BridgeLogMetadata {
    pub log: L1MessengerL2ToL1Log,
    pub message: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct WithdrawalRequest {
    /// The receiver l1 address.
    pub address: BitcoinAddress<NetworkUnchecked>,
    /// The amount user will receive.
    pub amount: U256,
}

/// Corresponds to the following solidity event:
/// ```solidity
/// struct L2ToL1Log {
///     uint8 l2ShardId;
///     bool isService;
///     uint16 txNumberInBlock;
///     address sender;
///     bytes32 key;
///     bytes32 value;
/// }
/// ```
#[derive(Clone, Debug, Default)]
pub struct L1MessengerL2ToL1Log {
    /// l2ShardId The shard identifier, 0 - rollup, 1 - porter
    /// All other values are not used but are reserved for the future
    pub l2_shard_id: u8,
    /// isService A boolean flag that is part of the log along with `key`, `value`, and `sender` address.
    /// This field is required formally but does not have any special meaning
    pub is_service: bool,
    /// txNumberInBatch The L2 transaction number in a Batch, in which the log was sent
    pub tx_number_in_block: u16,
    /// sender The L2 address which sent the log
    pub sender: H160,
    /// key The 32 bytes of information that was sent in the log
    pub key: H256,
    /// value The 32 bytes of information that was sent in the log
    pub value: H256,
}

impl L1MessengerL2ToL1Log {
    pub fn encode_packed(&self) -> Vec<u8> {
        let mut res: Vec<u8> = vec![];
        res.push(self.l2_shard_id);
        res.push(self.is_service as u8);
        res.extend_from_slice(&self.tx_number_in_block.to_be_bytes());
        res.extend_from_slice(self.sender.as_bytes());
        res.extend(u256_to_bytes_be(&U256::from_big_endian(&self.key.0)));
        res.extend(u256_to_bytes_be(&U256::from_big_endian(&self.value.0)));
        res
    }

    pub fn decode_packed<R: Read>(reader: &mut R) -> anyhow::Result<Self> {
        // Read `l2_shard_id` (1 byte)
        let l2_shard_id = reader.read_u8().context("Failed to read l2_shard_id")?;

        // Read `is_service` (1 byte, a boolean stored as 0 or 1)
        let is_service_byte = reader.read_u8().context("Failed to read is_service byte")?;
        let is_service = is_service_byte != 0; // 0 -> false, non-zero -> true

        // Read `tx_number_in_block` (2 bytes, u16)
        let tx_number_in_block = reader
            .read_u16::<BigEndian>()
            .context("Failed to read tx_number_in_block")?;

        // Read `sender` (address is 20 bytes)
        let mut sender_bytes = [0u8; 20];
        reader
            .read_exact(&mut sender_bytes)
            .context("Failed to read sender address")?;
        let sender = Address::from(sender_bytes);

        // Read `key` (U256 is 32 bytes)
        let key_bytes = _read_bytes(reader, 32).context("Failed to read key bytes")?;
        let key = u256_to_h256(U256::from_big_endian(&key_bytes));

        // Read `value` (U256 is 32 bytes)
        let value_bytes = _read_bytes(reader, 32).context("Failed to read value bytes")?;
        let value = u256_to_h256(U256::from_big_endian(&value_bytes));

        Ok(L1MessengerL2ToL1Log {
            l2_shard_id,
            is_service,
            tx_number_in_block,
            sender,
            key,
            value,
        })
    }
}

/// Helper function to read a specific number of bytes
fn _read_bytes<R: Read>(reader: &mut R, num_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let mut buffer = vec![0u8; num_bytes];
    reader.read_exact(&mut buffer)?;
    Ok(buffer)
}
