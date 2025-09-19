use std::str::FromStr;

use zksync_basic_types::{Address, PriorityOpId, H160, U256};
use zksync_system_constants::REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE;
use zksync_utils::address_to_u256;

use super::{
    priority_id::ViaPriorityOpId, L1Tx, L1TxCommonData, OpProcessingType, PriorityQueueType,
};
use crate::{
    abi::L2CanonicalTransaction, helpers::unix_timestamp_ms, Execute, PRIORITY_OPERATION_L2_TX_TYPE,
};

/// Eth 18 decimals - BTC 8 decimals
const MANTISSA: u64 = 10_000_000_000;

/// Deposit default L2 gas price.
const MAX_FEE_PER_GAS: u64 = 120_000_000;

/// Gas limit to required to execute a deposit.
const GAS_LIMIT: u64 = 300_000;

/// Max system contracts kernel space address.
const MAX_SYSTEM_CONTRACT_ADDRESS: &str = "0x000000000000000000000000000000000000ffff";

#[derive(Debug, Clone)]

pub struct ViaL1Deposit {
    pub l2_receiver_address: Address,
    pub amount: u64,
    pub calldata: Vec<u8>,
    pub l1_block_number: u64,
    pub tx_index: usize,
    pub output_vout: usize,
}

impl ViaL1Deposit {
    pub fn is_valid_deposit(&self) -> bool {
        if self.l2_receiver_address <= H160::from_str(MAX_SYSTEM_CONTRACT_ADDRESS).unwrap() {
            return false;
        }

        // CHeck if the amount can cover the transaction cost.
        let gas_fee = U256::from(GAS_LIMIT) * U256::from(MAX_FEE_PER_GAS);
        self.value() >= gas_fee
    }

    pub fn l1_tx(&self) -> Option<L1Tx> {
        if !self.is_valid_deposit() {
            return None;
        }
        Some(L1Tx::from(self.clone()))
    }

    fn value(&self) -> U256 {
        U256::from(self.amount) * U256::from(MANTISSA)
    }

    pub fn priority_id(&self) -> PriorityOpId {
        PriorityOpId(
            ViaPriorityOpId::new(
                self.l1_block_number,
                self.tx_index as u64,
                self.output_vout as u64,
            )
            .raw(),
        )
    }
}

impl From<ViaL1Deposit> for L1Tx {
    fn from(deposit: ViaL1Deposit) -> Self {
        let value = deposit.value();

        let l2_tx = L2CanonicalTransaction {
            tx_type: PRIORITY_OPERATION_L2_TX_TYPE.into(),
            from: address_to_u256(&deposit.l2_receiver_address),
            to: address_to_u256(&deposit.l2_receiver_address),
            gas_limit: U256::from(GAS_LIMIT),
            gas_per_pubdata_byte_limit: U256::from(REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE),
            max_fee_per_gas: U256::from(MAX_FEE_PER_GAS),
            max_priority_fee_per_gas: U256::zero(),
            paymaster: U256::zero(),
            nonce: deposit.priority_id().0.into(),
            value: U256::zero(),
            reserved: [
                value,
                address_to_u256(&deposit.l2_receiver_address),
                U256::zero(),
                U256::zero(),
            ],
            data: vec![],
            signature: vec![],
            factory_deps: vec![],
            paymaster_input: vec![],
            reserved_dynamic: vec![],
        };

        Self {
            execute: Execute {
                contract_address: Some(deposit.l2_receiver_address),
                calldata: vec![],
                value: U256::zero(),
                factory_deps: vec![],
            },
            common_data: L1TxCommonData {
                sender: deposit.l2_receiver_address,
                serial_id: deposit.priority_id(),
                layer_2_tip_fee: U256::zero(),
                full_fee: U256::zero(),
                max_fee_per_gas: U256::from(MAX_FEE_PER_GAS),
                gas_limit: U256::from(GAS_LIMIT),
                gas_per_pubdata_limit: U256::from(REQUIRED_L1_TO_L2_GAS_PER_PUBDATA_BYTE),
                op_processing_type: OpProcessingType::Common,
                priority_queue_type: PriorityQueueType::Deque,
                canonical_tx_hash: l2_tx.hash(),
                to_mint: value,
                refund_recipient: deposit.l2_receiver_address,
                eth_block: deposit.l1_block_number,
            },
            received_timestamp_ms: unix_timestamp_ms(),
        }
    }
}
