use bitcoin::policy::MAX_STANDARD_TX_WEIGHT;

/// approximate size per input
///
/// base_size: 41 bytes
/// Previous txid: 32 bytes
/// Previous output index: 4 bytes
/// Script length: 1 byte (0x00)
/// ScriptSig: 0 bytes (empty)
/// Sequence: 4 bytes
pub const INPUT_BASE_SIZE: u64 = 41_u64;

/// Witness
/// MuSig2 signature: 65 bytes
/// Signature 64 bytes
/// Signature type 1 Byte
pub const INPUT_WITNESS_SIZE: u64 = 65_u64;
pub const OUTPUT_SIZE: u64 = 34_u64;
pub const OP_RETURN_SIZE: u64 = 68_u64;

// Transaction overhead (version + input_count + output_count + locktime)
pub const TX_OVERHEAD: u64 = 10;
// Witness overhead (marker + flag)
pub const WITNESS_OVERHEAD: u64 = 2;

pub const INPUT_WEIGHT: u64 = (INPUT_BASE_SIZE * 4) + INPUT_WITNESS_SIZE;

pub const OUTPUT_WEIGHT: u64 = OUTPUT_SIZE * 4;

pub const FIXED_OVERHEAD_WEIGHT: u64 = (TX_OVERHEAD + OP_RETURN_SIZE) * 4 + WITNESS_OVERHEAD;

pub const AVAILABLE_WEIGHT: u64 = MAX_STANDARD_TX_WEIGHT as u64 - FIXED_OVERHEAD_WEIGHT;
