use std::io::{Cursor, Read};

use anyhow::Context;
use byteorder::{BigEndian, ReadBytesExt};

use crate::types::L1MessengerL2ToL1Log;

#[derive(Debug, Clone, Default)]
pub struct Pubdata {
    pub user_logs: Vec<L1MessengerL2ToL1Log>,
    pub l2_to_l1_messages: Vec<Vec<u8>>,
}

impl Pubdata {
    pub fn encode_pubdata(self) -> Vec<u8> {
        let mut l1_messenger_pubdata = vec![];

        // Encoding user L2->L1 logs.
        // Format: `[(numberOfL2ToL1Logs as u32) || l2tol1logs[1] || ... || l2tol1logs[n]]`
        l1_messenger_pubdata.extend((self.user_logs.len() as u32).to_be_bytes());
        for l2tol1log in self.user_logs {
            l1_messenger_pubdata.extend(l2tol1log.encode_packed());
        }

        // Encoding L2->L1 messages
        // Format: `[(numberOfMessages as u32) || (messages[1].len() as u32) || messages[1] || ... || (messages[n].len() as u32) || messages[n]]`
        l1_messenger_pubdata.extend((self.l2_to_l1_messages.len() as u32).to_be_bytes());
        for message in self.l2_to_l1_messages {
            l1_messenger_pubdata.extend((message.len() as u32).to_be_bytes());
            l1_messenger_pubdata.extend(message);
        }

        l1_messenger_pubdata
    }

    pub fn decode_pubdata(pubdata: Vec<u8>) -> anyhow::Result<Pubdata> {
        let mut cursor = Cursor::new(pubdata);
        let mut user_logs = Vec::new();
        let mut l2_to_l1_messages = Vec::new();

        // Decode user L2->L1 logs
        let num_user_logs = cursor
            .read_u32::<BigEndian>()
            .context("Failed to decode num user logs")? as usize;
        for _ in 0..num_user_logs {
            let log = L1MessengerL2ToL1Log::decode_packed(&mut cursor)?;
            user_logs.push(log);
        }

        // Decode L2->L1 messages
        let num_messages = cursor.read_u32::<BigEndian>()? as usize;
        for _ in 0..num_messages {
            let message_len = cursor.read_u32::<BigEndian>()? as usize;
            let mut message = vec![0u8; message_len];
            cursor
                .read_exact(&mut message)
                .context("Read l2 to l1 message")?;
            l2_to_l1_messages.push(message);
        }

        Ok(Pubdata {
            user_logs,
            l2_to_l1_messages,
        })
    }
}

/// Helper function to read a specific number of bytes
fn read_bytes<R: Read>(reader: &mut R, num_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let mut buffer = vec![0u8; num_bytes];
    reader.read_exact(&mut buffer)?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use hex::encode;
    use rand;
    use zksync_types::{web3::keccak256, Address, H256};

    use super::*;
    use crate::types::L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR;

    fn generate_random_hex(len: usize) -> String {
        // Generate random bytes
        let random_bytes: Vec<u8> = (0..len).map(|_| rand::random::<u8>()).collect();

        // Convert bytes to hex and return it
        encode(random_bytes)
    }

    #[test]
    fn test_decode_l1_messager_l2_to_l1_log() {
        let message = L1MessengerL2ToL1Log {
            l2_shard_id: 0,
            is_service: true,
            tx_number_in_block: 5,
            sender: Address::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR).unwrap(),
            key: H256::random(),
            value: H256::random(),
        };
        let encoded_messages = message.encode_packed();

        let mut cursor = Cursor::new(encoded_messages);
        let decoded = L1MessengerL2ToL1Log::decode_packed(&mut cursor).unwrap();
        assert_eq!(message.l2_shard_id, decoded.l2_shard_id);
        assert_eq!(message.is_service, decoded.is_service);
        assert_eq!(message.tx_number_in_block, decoded.tx_number_in_block);
        assert_eq!(message.sender, decoded.sender);
        assert_eq!(message.key, decoded.key);
        assert_eq!(message.value, decoded.value);
    }

    #[test]
    fn test_decode_pubdata_with_single_l1_messager_l2_to_l1_log() {
        let message = L1MessengerL2ToL1Log {
            l2_shard_id: 0,
            is_service: true,
            tx_number_in_block: 5,
            sender: Address::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR).unwrap(),
            key: H256::random(),
            value: H256::random(),
        };

        let pubdata = Pubdata {
            user_logs: vec![message.clone()],
            l2_to_l1_messages: vec![hex::decode("deadbeef").unwrap()],
        };

        let encoded_pubdata = pubdata.encode_pubdata();
        let pubdata_input = Pubdata::decode_pubdata(encoded_pubdata).unwrap();

        let decoded_message = pubdata_input.user_logs[0].clone();
        assert_eq!(pubdata_input.user_logs.len(), 1);
        assert_eq!(decoded_message.l2_shard_id, message.clone().l2_shard_id);
        assert_eq!(decoded_message.is_service, message.clone().is_service);
        assert_eq!(
            decoded_message.tx_number_in_block,
            message.clone().tx_number_in_block
        );
        assert_eq!(decoded_message.sender, message.clone().sender);
        assert_eq!(decoded_message.key, message.clone().key);
        assert_eq!(decoded_message.value, message.clone().value);
    }

    #[test]
    fn test_decode_pubdata_with_many_l1_messager_l2_to_l1_log() {
        let len: usize = 5;
        let mut user_logs: Vec<L1MessengerL2ToL1Log> = Vec::new();
        let mut l2_to_l1_messages: Vec<Vec<u8>> = Vec::new();
        for _ in 0..len {
            let log = L1MessengerL2ToL1Log {
                l2_shard_id: 0,
                is_service: true,
                tx_number_in_block: 5,
                sender: Address::from_str(L2_BASE_TOKEN_SYSTEM_CONTRACT_ADDR).unwrap(),
                key: H256::from_str(&generate_random_hex(32)).unwrap(),
                value: H256::from_str(&generate_random_hex(32)).unwrap(),
            };
            user_logs.push(log.clone());
            l2_to_l1_messages.push(hex::decode("deadbeef").unwrap());
        }

        let pubdata = Pubdata {
            user_logs: user_logs.clone(),
            l2_to_l1_messages: l2_to_l1_messages,
        };

        let encoded_pubdata = pubdata.encode_pubdata();
        let pubdata_input = Pubdata::decode_pubdata(encoded_pubdata).unwrap();

        let decoded_logs = pubdata_input.user_logs.clone();
        let decoded_messages = pubdata_input.l2_to_l1_messages.clone();
        assert_eq!(pubdata_input.user_logs.len(), len);
        assert_eq!(pubdata_input.l2_to_l1_messages.len(), len);
        for i in 0..len {
            let decoded_log = decoded_logs[i].clone();
            let msg_log = user_logs[i].clone();

            assert_eq!(decoded_log.l2_shard_id, msg_log.clone().l2_shard_id);
            assert_eq!(decoded_log.is_service, msg_log.clone().is_service);
            assert_eq!(
                decoded_log.tx_number_in_block,
                msg_log.clone().tx_number_in_block
            );
            assert_eq!(decoded_log.sender, msg_log.clone().sender);
            assert_eq!(decoded_log.key, msg_log.clone().key);
            assert_eq!(decoded_log.value, msg_log.clone().value);

            // l2 to l1 message
            let decoded_message = decoded_messages[i].clone();
            assert_eq!(decoded_message, hex::decode("deadbeef").unwrap());
        }
    }

    #[test]
    fn test_decode_pubdata_with_single_real_l1_messager_l2_to_l1_log() {
        let input = "00000001000100000000000000000000000000000000000000008008000000000000000000000000000000000000000000000000000000000000800aa1fd131a17718668a78581197d19972abd907b7b343b9694e02246d18c3801c500000001000000506c0960f962637274317178326c6b30756e756b6d3830716d65706a703439687766397a36786e7a307337336b396a35360000000000000000000000000000000000000000000000000000000005f5e10000000000010001280400032c1818e4770f08c05b28829d7d5f9d401d492c7432c166dfecf4af04238ea323009d7042e8fb0f249338d18505e5ba1d4a546e9d21f47c847ca725ff53ac29f740ca1bbc31cc849a8092a36f9a321e17412dee200b956038af1c2dc83430a0e8b000d3e2c6760d91078e517a2cb882cd3c9551de3ab5f30d554d51b17e3744cf92b0cf368ce957aed709b985423cd3ba11615de01ecafa15eb9a11bc6cdef4f6327900436ef22b96a07224eb06f0eecfecc184033da7db2a5fb58f867f17298b896b55000000420901000000362205f5e1000000003721032b8b14000000382209216c140000003a8901000000000000000000000000000000170000003b8902000000000000000000000000000000170000003e890200000000000000000000000000000017";
        let encoded_pubdata = hex::decode(input).unwrap();
        let pubdata_input = Pubdata::decode_pubdata(encoded_pubdata).unwrap();

        let hash = keccak256(&pubdata_input.l2_to_l1_messages[0].clone());
        assert_eq!(H256::from(hash), pubdata_input.user_logs[0].value);
    }
}
