use anyhow::Result;
use bitcoin::Amount;

// Fee Estimation Constants
const VERSION_SIZE: usize = 4;
const INPUT_COUNT_SIZE: usize = 1;
const OUTPUT_COUNT_SIZE: usize = 1;
const LOCKTIME_SIZE: usize = 4;
const MAKER_FLAGS_SIZE: usize = 1; // 1/2

// p2wpkh input base size
// out point (36) The txid and vout index number of the output (UTXO) being spent
// scriptSig length  (1)
// scriptSig (0) for p2wpkh and p2tr the scriptSig is empty
// sequence number (4)
// Witness item count (1/4)
// witness item (27)
//     ( (73) size signature + (34) size public_key ) / 4
// 36 + 1 + 0 + 4 + 1 + 27 = 69
const P2WPKH_INPUT_BASE_SIZE: usize = 69;

// p2tr input base size
// out point (36) The txid and vout index number of the output (UTXO) being spent
// scriptSig length  (1)
// scriptSig (0) for p2wpkh and p2tr the scriptSig is empty
// sequence number (4)
// Witness item count (3)
// witness item (17)
//     ( 65) size schnorr_signature / 4
// * rest of the witness items size is calculated based on the witness size
// 36 + 1 + 0 + 4 + 3 + 17 = 61
const P2TR_INPUT_BASE_SIZE: usize = 61;

// p2wpkh output base size
// value (8)
// scriptPubKey length (1)
// scriptPubKey (p2wpkh: 25)
// 8 + 1 + 25 = 34
const P2WPKH_OUTPUT_BASE_SIZE: usize = 34;

// p2tr output base size
// value (8)
// scriptPubKey length (1)
// scriptPubKey (p2tr: 34)
// 8 + 1 + 34 = 43
const P2TR_OUTPUT_BASE_SIZE: usize = 43;

pub struct InscriberFeeCalculator {}

impl InscriberFeeCalculator {
    fn estimate_transaction_size(
        p2wpkh_inputs_count: u32,
        p2tr_inputs_count: u32,
        p2wpkh_outputs_count: u32,
        p2tr_outputs_count: u32,
        p2tr_witness_sizes: Vec<usize>,
    ) -> usize {
        // https://bitcoinops.org/en/tools/calc-size/
        // https://en.bitcoin.it/wiki/Protocol_documentation#Common_structures
        // https://btcinformation.org/en/developer-reference#p2p-network

        assert!(p2tr_inputs_count == p2tr_witness_sizes.len() as u32);

        let base_size =
            VERSION_SIZE + INPUT_COUNT_SIZE + OUTPUT_COUNT_SIZE + LOCKTIME_SIZE + MAKER_FLAGS_SIZE;

        let p2wpkh_input_size = P2WPKH_INPUT_BASE_SIZE * p2wpkh_inputs_count as usize;

        let mut p2tr_input_size = 0;

        for witness_size in p2tr_witness_sizes {
            p2tr_input_size += P2TR_INPUT_BASE_SIZE + witness_size;
        }

        let p2wpkh_output_size = P2WPKH_OUTPUT_BASE_SIZE * p2wpkh_outputs_count as usize;

        let p2tr_output_size = P2TR_OUTPUT_BASE_SIZE * p2tr_outputs_count as usize;

        base_size + p2wpkh_input_size + p2tr_input_size + p2wpkh_output_size + p2tr_output_size
    }

    pub fn estimate_fee(
        p2wpkh_inputs_count: u32,
        p2tr_inputs_count: u32,
        p2wpkh_outputs_count: u32,
        p2tr_outputs_count: u32,
        p2tr_witness_sizes: Vec<usize>,
        fee_rate: u64,
    ) -> Result<Amount> {
        let transaction_size = Self::estimate_transaction_size(
            p2wpkh_inputs_count,
            p2tr_inputs_count,
            p2wpkh_outputs_count,
            p2tr_outputs_count,
            p2tr_witness_sizes,
        );

        let fee = transaction_size as u64 * fee_rate;

        let fee = Amount::from_sat(fee);

        Ok(fee)
    }
}
