use zksync_types::{
    fee_model::{BatchFeeInput, L1PeggedBatchFeeModelInput, PubdataIndependentBatchFeeModelInput},
    vm::VmVersion,
    U256,
};

pub use self::deduplicator::{ModifiedSlot, StorageWritesDeduplicator};
use crate::interface::L1BatchEnv;

pub(crate) mod bytecode;
mod deduplicator;
pub(crate) mod events;

/// Calculates the base fee and gas per pubdata for the given L1 gas price.
pub fn derive_base_fee_and_gas_per_pubdata(
    batch_fee_input: BatchFeeInput,
    vm_version: VmVersion,
) -> (u64, u64) {
    match vm_version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::vm_with_bootloader::derive_base_fee_and_gas_per_pubdata(
                batch_fee_input.into_l1_pegged(),
            )
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::vm_with_bootloader::derive_base_fee_and_gas_per_pubdata(
                batch_fee_input.into_l1_pegged(),
            )
        }
        VmVersion::Vm1_3_2 => {
            crate::vm_1_3_2::vm_with_bootloader::derive_base_fee_and_gas_per_pubdata(
                batch_fee_input.into_l1_pegged(),
            )
        }
        VmVersion::VmVirtualBlocks => {
            crate::vm_virtual_blocks::utils::fee::derive_base_fee_and_gas_per_pubdata(
                batch_fee_input.into_l1_pegged(),
            )
        }
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::utils::fee::derive_base_fee_and_gas_per_pubdata(
                batch_fee_input.into_l1_pegged(),
            )
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::utils::fee::derive_base_fee_and_gas_per_pubdata(
                batch_fee_input.into_l1_pegged(),
            )
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::utils::fee::derive_base_fee_and_gas_per_pubdata(
            batch_fee_input.into_pubdata_independent(),
        ),
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::utils::fee::derive_base_fee_and_gas_per_pubdata(
            batch_fee_input.into_pubdata_independent(),
        ),
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::utils::fee::derive_base_fee_and_gas_per_pubdata(
                batch_fee_input.into_pubdata_independent(),
            )
        }
        VmVersion::VmBitcoin1_0_0 => {
            crate::vm_latest::utils::fee::derive_base_fee_and_gas_per_pubdata(
                batch_fee_input.into_pubdata_independent(),
            )
        }
    }
}

pub fn get_batch_base_fee(l1_batch_env: &L1BatchEnv, vm_version: VmVersion) -> u64 {
    match vm_version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::vm_with_bootloader::get_batch_base_fee(l1_batch_env)
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::vm_with_bootloader::get_batch_base_fee(l1_batch_env)
        }
        VmVersion::Vm1_3_2 => crate::vm_1_3_2::vm_with_bootloader::get_batch_base_fee(l1_batch_env),
        VmVersion::VmVirtualBlocks => {
            crate::vm_virtual_blocks::utils::fee::get_batch_base_fee(l1_batch_env)
        }
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::utils::fee::get_batch_base_fee(l1_batch_env)
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::utils::fee::get_batch_base_fee(l1_batch_env)
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::utils::fee::get_batch_base_fee(l1_batch_env),
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::utils::fee::get_batch_base_fee(l1_batch_env),
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::utils::fee::get_batch_base_fee(l1_batch_env)
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::utils::fee::get_batch_base_fee(l1_batch_env),
    }
}

/// Changes the batch fee input so that the expected gas per pubdata is smaller than or the `tx_gas_per_pubdata_limit`.
pub fn adjust_pubdata_price_for_tx(
    batch_fee_input: BatchFeeInput,
    tx_gas_per_pubdata_limit: U256,
    max_base_fee: Option<U256>,
    vm_version: VmVersion,
) -> BatchFeeInput {
    // If no max base fee was provided, we just use the maximal one for convenience.
    let max_base_fee = max_base_fee.unwrap_or(U256::MAX);
    let desired_gas_per_pubdata =
        tx_gas_per_pubdata_limit.min(get_max_gas_per_pubdata_byte(vm_version).into());

    let (current_base_fee, current_gas_per_pubdata) =
        derive_base_fee_and_gas_per_pubdata(batch_fee_input, vm_version);

    if U256::from(current_gas_per_pubdata) <= desired_gas_per_pubdata
        && U256::from(current_base_fee) <= max_base_fee
    {
        // gas per pubdata is already smaller than or equal to `tx_gas_per_pubdata_limit`.
        return batch_fee_input;
    }

    match batch_fee_input {
        BatchFeeInput::L1Pegged(fee_input) => {
            let current_l2_fair_gas_price = U256::from(fee_input.fair_l2_gas_price);
            let fair_l2_gas_price = if max_base_fee < current_l2_fair_gas_price {
                max_base_fee
            } else {
                current_l2_fair_gas_price
            };

            // `gasPerPubdata = ceil(17 * l1gasprice / fair_l2_gas_price)`
            // `gasPerPubdata <= 17 * l1gasprice / fair_l2_gas_price + 1`
            // `fair_l2_gas_price(gasPerPubdata - 1) / 17 <= l1gasprice`
            let new_l1_gas_price =
                fair_l2_gas_price * (desired_gas_per_pubdata - U256::from(1u32)) / U256::from(17);

            BatchFeeInput::L1Pegged(L1PeggedBatchFeeModelInput {
                l1_gas_price: new_l1_gas_price.as_u64(),
                fair_l2_gas_price: fair_l2_gas_price.as_u64(),
            })
        }
        BatchFeeInput::PubdataIndependent(fee_input) => {
            let current_l2_fair_gas_price = U256::from(fee_input.fair_l2_gas_price);
            let fair_l2_gas_price = if max_base_fee < current_l2_fair_gas_price {
                max_base_fee
            } else {
                current_l2_fair_gas_price
            };

            // `gasPerPubdata = ceil(fair_pubdata_price / fair_l2_gas_price)`
            // `gasPerPubdata <= fair_pubdata_price / fair_l2_gas_price + 1`
            // `fair_l2_gas_price(gasPerPubdata - 1) <= fair_pubdata_price`
            let new_fair_pubdata_price =
                fair_l2_gas_price * (desired_gas_per_pubdata - U256::from(1u32));

            BatchFeeInput::PubdataIndependent(PubdataIndependentBatchFeeModelInput {
                fair_pubdata_price: new_fair_pubdata_price.as_u64(),
                fair_l2_gas_price: fair_l2_gas_price.as_u64(),
                ..fee_input
            })
        }
    }
}

pub fn derive_overhead(
    gas_limit: u64,
    gas_price_per_pubdata: u32,
    encoded_len: usize,
    tx_type: u8,
    vm_version: VmVersion,
) -> u32 {
    match vm_version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::transaction_data::derive_overhead(
                gas_limit,
                gas_price_per_pubdata,
                encoded_len,
            )
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::transaction_data::derive_overhead(
                gas_limit,
                gas_price_per_pubdata,
                encoded_len,
                crate::vm_m6::transaction_data::OverheadCoefficients::from_tx_type(tx_type),
            )
        }
        VmVersion::Vm1_3_2 => crate::vm_1_3_2::transaction_data::derive_overhead(
            gas_limit,
            gas_price_per_pubdata,
            encoded_len,
            crate::vm_1_3_2::transaction_data::OverheadCoefficients::from_tx_type(tx_type),
        ),
        VmVersion::VmVirtualBlocks => crate::vm_virtual_blocks::utils::overhead::derive_overhead(
            gas_limit,
            gas_price_per_pubdata,
            encoded_len,
            crate::vm_virtual_blocks::utils::overhead::OverheadCoefficients::from_tx_type(tx_type),
        ),
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::utils::overhead::derive_overhead(
                gas_limit,
                gas_price_per_pubdata,
                encoded_len,
                crate::vm_refunds_enhancement::utils::overhead::OverheadCoefficients::from_tx_type(
                    tx_type,
                ),
            )
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::utils::overhead::derive_overhead(
                gas_limit,
                gas_price_per_pubdata,
                encoded_len,
                crate::vm_boojum_integration::utils::overhead::OverheadCoefficients::from_tx_type(
                    tx_type,
                ),
            )
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::utils::overhead::derive_overhead(encoded_len),
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::utils::overhead::derive_overhead(encoded_len),
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::utils::overhead::derive_overhead(encoded_len)
        }
        VmVersion::VmBitcoin1_0_0 => {
            crate::vm_latest::utils::overhead::derive_overhead(encoded_len)
        }
    }
}

pub fn get_bootloader_encoding_space(version: VmVersion) -> u32 {
    match version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::vm_with_bootloader::BOOTLOADER_TX_ENCODING_SPACE
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::vm_with_bootloader::BOOTLOADER_TX_ENCODING_SPACE
        }
        VmVersion::Vm1_3_2 => crate::vm_1_3_2::vm_with_bootloader::BOOTLOADER_TX_ENCODING_SPACE,
        VmVersion::VmVirtualBlocks => {
            crate::vm_virtual_blocks::constants::BOOTLOADER_TX_ENCODING_SPACE
        }
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::constants::BOOTLOADER_TX_ENCODING_SPACE
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::constants::BOOTLOADER_TX_ENCODING_SPACE
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::constants::BOOTLOADER_TX_ENCODING_SPACE,
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::constants::BOOTLOADER_TX_ENCODING_SPACE,
        VmVersion::Vm1_5_0SmallBootloaderMemory => {
            crate::vm_latest::constants::get_bootloader_tx_encoding_space(
                crate::vm_latest::MultiVMSubversion::SmallBootloaderMemory,
            )
        }
        VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::get_bootloader_tx_encoding_space(
                crate::vm_latest::MultiVMSubversion::IncreasedBootloaderMemory,
            )
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::constants::get_bootloader_tx_encoding_space(
            crate::vm_latest::MultiVMSubversion::IncreasedBootloaderMemory,
        ),
    }
}

pub fn get_bootloader_max_txs_in_batch(version: VmVersion) -> usize {
    match version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::vm_with_bootloader::MAX_TXS_IN_BLOCK
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::vm_with_bootloader::MAX_TXS_IN_BLOCK
        }
        VmVersion::Vm1_3_2 => crate::vm_1_3_2::vm_with_bootloader::MAX_TXS_IN_BLOCK,
        VmVersion::VmVirtualBlocks => crate::vm_virtual_blocks::constants::MAX_TXS_IN_BLOCK,
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::constants::MAX_TXS_IN_BLOCK
        }
        VmVersion::VmBoojumIntegration => crate::vm_boojum_integration::constants::MAX_TXS_IN_BLOCK,
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::constants::MAX_TXS_IN_BATCH,
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::constants::MAX_TXS_IN_BATCH,
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::MAX_TXS_IN_BATCH
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::constants::MAX_TXS_IN_BATCH,
    }
}

pub fn gas_bootloader_batch_tip_overhead(version: VmVersion) -> u32 {
    match version {
        VmVersion::M5WithRefunds
        | VmVersion::M5WithoutRefunds
        | VmVersion::M6Initial
        | VmVersion::M6BugWithCompressionFixed
        | VmVersion::Vm1_3_2
        | VmVersion::VmVirtualBlocks
        | VmVersion::VmVirtualBlocksRefundsEnhancement => {
            // For these versions the overhead has not been calculated and it has not been used with those versions.
            0
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::constants::BOOTLOADER_BATCH_TIP_OVERHEAD
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::constants::BOOTLOADER_BATCH_TIP_OVERHEAD,
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::constants::BOOTLOADER_BATCH_TIP_OVERHEAD,
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::BOOTLOADER_BATCH_TIP_OVERHEAD
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::constants::BOOTLOADER_BATCH_TIP_OVERHEAD,
    }
}

pub fn circuit_statistics_bootloader_batch_tip_overhead(version: VmVersion) -> usize {
    match version {
        VmVersion::M5WithRefunds
        | VmVersion::M5WithoutRefunds
        | VmVersion::M6Initial
        | VmVersion::M6BugWithCompressionFixed
        | VmVersion::Vm1_3_2
        | VmVersion::VmVirtualBlocks
        | VmVersion::VmVirtualBlocksRefundsEnhancement
        | VmVersion::VmBoojumIntegration
        | VmVersion::Vm1_4_1 => {
            // For these versions the overhead has not been calculated and it has not been used with those versions.
            0
        }
        VmVersion::Vm1_4_2 => {
            crate::vm_1_4_2::constants::BOOTLOADER_BATCH_TIP_CIRCUIT_STATISTICS_OVERHEAD as usize
        }
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::BOOTLOADER_BATCH_TIP_CIRCUIT_STATISTICS_OVERHEAD as usize
        }
        VmVersion::VmBitcoin1_0_0 => {
            crate::vm_latest::constants::BOOTLOADER_BATCH_TIP_CIRCUIT_STATISTICS_OVERHEAD as usize
        }
    }
}

pub fn execution_metrics_bootloader_batch_tip_overhead(version: VmVersion) -> usize {
    match version {
        VmVersion::M5WithRefunds
        | VmVersion::M5WithoutRefunds
        | VmVersion::M6Initial
        | VmVersion::M6BugWithCompressionFixed
        | VmVersion::Vm1_3_2
        | VmVersion::VmVirtualBlocks
        | VmVersion::VmVirtualBlocksRefundsEnhancement
        | VmVersion::VmBoojumIntegration
        | VmVersion::Vm1_4_1 => {
            // For these versions the overhead has not been calculated and it has not been used with those versions.
            0
        }
        VmVersion::Vm1_4_2 => {
            crate::vm_1_4_2::constants::BOOTLOADER_BATCH_TIP_METRICS_SIZE_OVERHEAD as usize
        }
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::BOOTLOADER_BATCH_TIP_METRICS_SIZE_OVERHEAD as usize
        }
        VmVersion::VmBitcoin1_0_0 => {
            crate::vm_latest::constants::BOOTLOADER_BATCH_TIP_METRICS_SIZE_OVERHEAD as usize
        }
    }
}

pub fn get_max_gas_per_pubdata_byte(version: VmVersion) -> u64 {
    match version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::vm_with_bootloader::MAX_GAS_PER_PUBDATA_BYTE
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::vm_with_bootloader::MAX_GAS_PER_PUBDATA_BYTE
        }
        VmVersion::Vm1_3_2 => crate::vm_1_3_2::vm_with_bootloader::MAX_GAS_PER_PUBDATA_BYTE,
        VmVersion::VmVirtualBlocks => crate::vm_virtual_blocks::constants::MAX_GAS_PER_PUBDATA_BYTE,
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::constants::MAX_GAS_PER_PUBDATA_BYTE
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::constants::MAX_GAS_PER_PUBDATA_BYTE
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::constants::MAX_GAS_PER_PUBDATA_BYTE,
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::constants::MAX_GAS_PER_PUBDATA_BYTE,
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::MAX_GAS_PER_PUBDATA_BYTE
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::constants::MAX_GAS_PER_PUBDATA_BYTE,
    }
}

pub fn get_used_bootloader_memory_bytes(version: VmVersion) -> usize {
    match version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::vm_with_bootloader::USED_BOOTLOADER_MEMORY_BYTES
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::vm_with_bootloader::USED_BOOTLOADER_MEMORY_BYTES
        }
        VmVersion::Vm1_3_2 => crate::vm_1_3_2::vm_with_bootloader::USED_BOOTLOADER_MEMORY_BYTES,
        VmVersion::VmVirtualBlocks => {
            crate::vm_virtual_blocks::constants::USED_BOOTLOADER_MEMORY_BYTES
        }
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::constants::USED_BOOTLOADER_MEMORY_BYTES
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::constants::USED_BOOTLOADER_MEMORY_BYTES
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::constants::USED_BOOTLOADER_MEMORY_BYTES,
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::constants::USED_BOOTLOADER_MEMORY_BYTES,
        VmVersion::Vm1_5_0SmallBootloaderMemory => {
            crate::vm_latest::constants::get_used_bootloader_memory_bytes(
                crate::vm_latest::MultiVMSubversion::SmallBootloaderMemory,
            )
        }
        VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::get_used_bootloader_memory_bytes(
                crate::vm_latest::MultiVMSubversion::IncreasedBootloaderMemory,
            )
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::constants::get_used_bootloader_memory_bytes(
            crate::vm_latest::MultiVMSubversion::IncreasedBootloaderMemory,
        ),
    }
}

pub fn get_used_bootloader_memory_words(version: VmVersion) -> usize {
    match version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::vm_with_bootloader::USED_BOOTLOADER_MEMORY_WORDS
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::vm_with_bootloader::USED_BOOTLOADER_MEMORY_WORDS
        }
        VmVersion::Vm1_3_2 => crate::vm_1_3_2::vm_with_bootloader::USED_BOOTLOADER_MEMORY_WORDS,
        VmVersion::VmVirtualBlocks => {
            crate::vm_virtual_blocks::constants::USED_BOOTLOADER_MEMORY_WORDS
        }
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::constants::USED_BOOTLOADER_MEMORY_WORDS
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::constants::USED_BOOTLOADER_MEMORY_WORDS
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::constants::USED_BOOTLOADER_MEMORY_WORDS,
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::constants::USED_BOOTLOADER_MEMORY_WORDS,
        VmVersion::Vm1_5_0SmallBootloaderMemory => {
            crate::vm_latest::constants::get_used_bootloader_memory_bytes(
                crate::vm_latest::MultiVMSubversion::SmallBootloaderMemory,
            )
        }
        VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::get_used_bootloader_memory_bytes(
                crate::vm_latest::MultiVMSubversion::IncreasedBootloaderMemory,
            )
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::constants::get_used_bootloader_memory_bytes(
            crate::vm_latest::MultiVMSubversion::IncreasedBootloaderMemory,
        ),
    }
}

pub fn get_max_batch_gas_limit(version: VmVersion) -> u64 {
    match version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::utils::BLOCK_GAS_LIMIT as u64
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::utils::BLOCK_GAS_LIMIT as u64
        }
        VmVersion::Vm1_3_2 => crate::vm_1_3_2::utils::BLOCK_GAS_LIMIT as u64,
        VmVersion::VmVirtualBlocks => crate::vm_virtual_blocks::constants::BLOCK_GAS_LIMIT as u64,
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::constants::BLOCK_GAS_LIMIT as u64
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::constants::BLOCK_GAS_LIMIT as u64
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::constants::BLOCK_GAS_LIMIT as u64,
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::constants::BLOCK_GAS_LIMIT as u64,
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::BATCH_GAS_LIMIT
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::constants::BATCH_GAS_LIMIT,
    }
}

pub fn get_eth_call_gas_limit(version: VmVersion) -> u64 {
    match version {
        VmVersion::M5WithRefunds | VmVersion::M5WithoutRefunds => {
            crate::vm_m5::utils::ETH_CALL_GAS_LIMIT as u64
        }
        VmVersion::M6Initial | VmVersion::M6BugWithCompressionFixed => {
            crate::vm_m6::utils::ETH_CALL_GAS_LIMIT as u64
        }
        VmVersion::Vm1_3_2 => crate::vm_1_3_2::utils::ETH_CALL_GAS_LIMIT as u64,
        VmVersion::VmVirtualBlocks => {
            crate::vm_virtual_blocks::constants::ETH_CALL_GAS_LIMIT as u64
        }
        VmVersion::VmVirtualBlocksRefundsEnhancement => {
            crate::vm_refunds_enhancement::constants::ETH_CALL_GAS_LIMIT as u64
        }
        VmVersion::VmBoojumIntegration => {
            crate::vm_boojum_integration::constants::ETH_CALL_GAS_LIMIT as u64
        }
        VmVersion::Vm1_4_1 => crate::vm_1_4_1::constants::ETH_CALL_GAS_LIMIT as u64,
        VmVersion::Vm1_4_2 => crate::vm_1_4_2::constants::ETH_CALL_GAS_LIMIT as u64,
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::ETH_CALL_GAS_LIMIT
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::constants::ETH_CALL_GAS_LIMIT,
    }
}

pub fn get_max_batch_base_layer_circuits(version: VmVersion) -> usize {
    match version {
        VmVersion::M5WithRefunds
        | VmVersion::M5WithoutRefunds
        | VmVersion::M6Initial
        | VmVersion::M6BugWithCompressionFixed
        | VmVersion::Vm1_3_2
        | VmVersion::VmVirtualBlocks
        | VmVersion::VmVirtualBlocksRefundsEnhancement
        | VmVersion::VmBoojumIntegration
        | VmVersion::Vm1_4_1
        | VmVersion::Vm1_4_2 => {
            // For pre-v1.4.2 the maximal number of circuits has not been calculated, but since
            // these are used only for replaying transactions, we'll reuse the same value as for v1.4.2.
            // We avoid providing `0` for the old versions to avoid potential errors when working with old versions.
            crate::vm_1_4_2::constants::MAX_BASE_LAYER_CIRCUITS
        }
        VmVersion::Vm1_5_0SmallBootloaderMemory | VmVersion::Vm1_5_0IncreasedBootloaderMemory => {
            crate::vm_latest::constants::MAX_BASE_LAYER_CIRCUITS
        }
        VmVersion::VmBitcoin1_0_0 => crate::vm_latest::constants::MAX_BASE_LAYER_CIRCUITS,
    }
}

/// Holds information about number of cycles used per circuit type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct CircuitCycleStatistic {
    pub main_vm_cycles: u32,
    pub ram_permutation_cycles: u32,
    pub storage_application_cycles: u32,
    pub storage_sorter_cycles: u32,
    pub code_decommitter_cycles: u32,
    pub code_decommitter_sorter_cycles: u32,
    pub log_demuxer_cycles: u32,
    pub events_sorter_cycles: u32,
    pub keccak256_cycles: u32,
    pub ecrecover_cycles: u32,
    pub sha256_cycles: u32,
    pub secp256k1_verify_cycles: u32,
    pub transient_storage_checker_cycles: u32,
}
