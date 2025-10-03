use bitcoin::{Amount, TxOut};

use crate::{
    constants::{
        INPUT_BASE_SIZE, INPUT_WITNESS_SIZE, OP_RETURN_SIZE, OUTPUT_SIZE, TX_OVERHEAD,
        WITNESS_OVERHEAD,
    },
    types::{TransactionOutput, TransactionWithFee},
};

pub trait FeeStrategy: Send + Sync {
    fn estimate_fee(
        &self,
        input_count: u32,
        output_count: u32,
        fee_rate: u64,
    ) -> anyhow::Result<Amount> {
        let input_size =
            (WITNESS_OVERHEAD + INPUT_BASE_SIZE + INPUT_WITNESS_SIZE) * u64::from(input_count);
        // approximate size per output +2 (+1 for potential change)
        let output_size = OUTPUT_SIZE * u64::from(output_count + 1);

        let total_size = TX_OVERHEAD + input_size + output_size + OP_RETURN_SIZE;
        let fee = fee_rate * total_size;

        // Ensure fee is divisible by output_count to avoid decimals when splitting
        let output_count_u64 = std::cmp::max(output_count, 1) as u64;
        let remainder = fee % output_count_u64;
        let adjusted_fee = if remainder == 0 {
            fee
        } else {
            fee + (output_count_u64 - remainder)
        };

        Ok(Amount::from_sat(adjusted_fee))
    }

    fn apply_fee_to_outputs(
        &self,
        outputs: Vec<TransactionOutput>,
        input_count: u32,
        fee_rate: u64,
    ) -> anyhow::Result<TransactionWithFee>;
}

pub struct WithdrawalFeeStrategy {}

impl WithdrawalFeeStrategy {
    pub fn new() -> Self {
        Self {}
    }
}

impl FeeStrategy for WithdrawalFeeStrategy {
    fn apply_fee_to_outputs(
        &self,
        mut outputs: Vec<TransactionOutput>,
        input_count: u32,
        fee_rate: u64,
    ) -> anyhow::Result<TransactionWithFee> {
        loop {
            let fee = self.estimate_fee(input_count, outputs.len() as u32, fee_rate)?;
            if outputs.is_empty() {
                return Ok(TransactionWithFee {
                    outputs_with_fees: vec![],
                    fee,
                    total_value_needed: Amount::ZERO,
                });
            }

            let fee_per_user = Amount::from_sat(fee.to_sat() / outputs.len() as u64);
            let mut total_value_needed = Amount::ZERO;
            let mut valid_outputs_count = 0;

            for output in &outputs {
                if output.output.value >= fee_per_user {
                    valid_outputs_count += 1;
                    total_value_needed += output.output.value - fee_per_user;
                }
            }

            if valid_outputs_count == outputs.len() {
                let mut new_outputs_with_fee = Vec::with_capacity(outputs.len());

                for output in outputs {
                    let output_with_fee = TransactionOutput {
                        output: TxOut {
                            script_pubkey: output.output.script_pubkey,
                            value: output.output.value - fee_per_user,
                        },
                        op_return_data: output.op_return_data,
                    };
                    new_outputs_with_fee.push(output_with_fee);
                }

                return Ok(TransactionWithFee {
                    outputs_with_fees: new_outputs_with_fee,
                    fee,
                    total_value_needed,
                });
            }

            outputs.retain(|output| output.output.value >= fee_per_user);
        }
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::{Amount, ScriptBuf, TxOut};

    use super::*;

    fn dummy_output(value: Amount) -> TxOut {
        TxOut {
            value,
            script_pubkey: ScriptBuf::new(),
        }
    }

    #[test]
    fn test_fee_applied_equally_to_outputs() {
        let strategy = WithdrawalFeeStrategy::new();
        let amount = Amount::from_sat(10_000);
        let outputs = vec![
            TransactionOutput {
                output: dummy_output(amount),
                op_return_data: None,
            },
            TransactionOutput {
                output: dummy_output(amount),
                op_return_data: None,
            },
        ];

        let input_count = 2;
        let fee_rate = 2;

        let tx_fee = strategy
            .apply_fee_to_outputs(outputs.clone(), input_count, fee_rate)
            .expect("fee application failed");

        assert_eq!(tx_fee.outputs_with_fees.len(), 2);

        let expected_fee = strategy
            .estimate_fee(input_count, outputs.len() as u32, fee_rate)
            .unwrap();

        let fee_per_output = Amount::from_sat(expected_fee.to_sat() / 2);

        for output in tx_fee.outputs_with_fees {
            assert_eq!(output.output.value, amount - fee_per_output);
        }

        assert_eq!(tx_fee.fee, expected_fee);
    }

    #[test]
    fn test_small_output_is_removed() {
        let strategy = WithdrawalFeeStrategy::new();
        let amount1 = Amount::from_sat(10_000);
        let amount2 = Amount::from_sat(500);

        let outputs = vec![
            TransactionOutput {
                output: dummy_output(amount1),
                op_return_data: None,
            },
            TransactionOutput {
                output: dummy_output(amount2),
                op_return_data: None,
            },
        ]; // this one will be too small to cover fee

        let input_count = 2;
        let fee_rate = 10;

        let tx_fee = strategy
            .apply_fee_to_outputs(outputs.clone(), input_count, fee_rate)
            .expect("fee application failed");

        // One output should be removed
        assert_eq!(tx_fee.outputs_with_fees.len(), 1);
        assert!(tx_fee.outputs_with_fees[0].output.value < amount1);

        // Make sure fee is re-estimated with 1 output
        let expected_fee = strategy.estimate_fee(input_count, 1, fee_rate).unwrap();

        assert_eq!(tx_fee.fee, expected_fee);
    }

    #[test]
    fn test_no_outputs_can_pay_fee() {
        let strategy = WithdrawalFeeStrategy::new();
        let amount = Amount::from_sat(100);

        let outputs = vec![
            TransactionOutput {
                output: dummy_output(amount), // not enough to cover any reasonable fee
                op_return_data: None,
            },
            TransactionOutput {
                output: dummy_output(amount),
                op_return_data: None,
            },
        ];

        let input_count = 1;
        let fee_rate = 100;

        let tx_fee = strategy
            .apply_fee_to_outputs(outputs.clone(), input_count, fee_rate)
            .expect("fee application failed");
        let expected_fee = strategy
            .estimate_fee(input_count, tx_fee.outputs_with_fees.len() as u32, fee_rate)
            .unwrap();

        assert_eq!(tx_fee.outputs_with_fees.len(), 0);
        assert_eq!(tx_fee.total_value_needed, Amount::ZERO);
        assert_eq!(tx_fee.fee, expected_fee);
    }

    #[test]
    fn test_fee_zero_rate() {
        let strategy = WithdrawalFeeStrategy::new();
        let amount1 = Amount::from_sat(1000);
        let amount2 = Amount::from_sat(2000);
        let outputs = vec![
            TransactionOutput {
                output: dummy_output(amount1),
                op_return_data: None,
            },
            TransactionOutput {
                output: dummy_output(amount2),
                op_return_data: None,
            },
        ];

        let input_count = 1;
        let fee_rate = 0;

        let tx_fee = strategy
            .apply_fee_to_outputs(outputs.clone(), input_count, fee_rate)
            .expect("fee application failed");

        assert_eq!(tx_fee.outputs_with_fees.len(), 2);
        assert_eq!(tx_fee.outputs_with_fees[0].output.value, amount1);
        assert_eq!(tx_fee.outputs_with_fees[1].output.value, amount2);
        assert_eq!(tx_fee.fee.to_sat(), 0);
    }

    #[test]
    fn test_estimate_fee_multiple_cases() {
        // Test cases: (input_count, output_count, fee_rate, expected_fee)
        let test_cases = vec![
            // Case 1: Fee already divisible
            (2, 3, 10, 4761), // 476 bytes * 10 = 4760, remainder 2, adjusted to 4761
            // Case 2: Fee needs adjustment
            (1, 4, 15, 5432), // 362 bytes * 15 = 5430, remainder 2, adjusted to 5432
            // Case 3: Single output (edge case)
            (1, 1, 20, 5200), // 260 bytes * 20 = 5200, remainder 0, no adjustment
            // Case 4: Perfect divisibility
            (3, 5, 8, 5540), // 692 bytes * 8 = 5536, remainder 1, adjusted to 5540
            // Case 5: High fee rate scenario
            (2, 7, 50, 30604), // 612 bytes * 50 = 30600, remainder 3, adjusted to 30604
            // Case 6: Zero outputs (edge case - should use max(1))
            (1, 0, 10, 2260), // 260 bytes * 10 = 2260, remainder 0, no adjustment
            // Case 7: Large number of inputs and outputs
            (5, 10, 25, 28950), // 1158 bytes * 25 = 28950, remainder 0, no adjustment
            // Case 8: Minimum fee rate
            (1, 2, 1, 294), // 294 bytes * 1 = 294, remainder 0, no adjustment
        ];

        // Mock struct for testing (replace with your actual struct)
        struct MockFeeEstimator;

        impl FeeStrategy for MockFeeEstimator {
            fn apply_fee_to_outputs(
                &self,
                _: Vec<TransactionOutput>,
                _: u32,
                _: u64,
            ) -> anyhow::Result<TransactionWithFee> {
                Ok(TransactionWithFee::default())
            }
        }

        let estimator = MockFeeEstimator;

        for (i, (input_count, output_count, fee_rate, expected_fee)) in
            test_cases.iter().enumerate()
        {
            let result = estimator.estimate_fee(*input_count, *output_count, *fee_rate);

            assert!(
                result.is_ok(),
                "Test case {} failed: estimate_fee returned error: {:?}",
                i + 1,
                result.err()
            );

            let actual_fee = result.unwrap().to_sat();
            assert_eq!(
                actual_fee, *expected_fee,
                "Test case {} failed: input_count={}, output_count={}, fee_rate={}, expected={}, actual={}",
                i + 1, input_count, output_count, fee_rate, expected_fee, actual_fee
            );

            // Verify the fee is divisible by output_count (unless output_count is 0)
            if *output_count > 0 {
                assert_eq!(
                    actual_fee % (*output_count as u64),
                    0,
                    "Test case {} failed: fee {} is not divisible by output_count {}",
                    i + 1,
                    actual_fee,
                    output_count
                );
            }
        }
    }
}
