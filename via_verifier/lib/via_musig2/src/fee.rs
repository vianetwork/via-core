use anyhow::Context as _;
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

    /// Removes fee-ineligible outputs in place while preserving survivor order.
    fn apply_fee_to_outputs(
        &self,
        outputs: &mut Vec<TransactionOutput>,
        input_count: u32,
        fee_rate: u64,
    ) -> anyhow::Result<TransactionWithFee>;
}

#[derive(Default)]
pub struct WithdrawalFeeStrategy;

impl WithdrawalFeeStrategy {
    pub const fn new() -> Self {
        Self
    }
}

impl FeeStrategy for WithdrawalFeeStrategy {
    fn apply_fee_to_outputs(
        &self,
        outputs: &mut Vec<TransactionOutput>,
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
            let previous_len = outputs.len();
            outputs.retain(|output| output.output.value > fee_per_user);
            if outputs.len() < previous_len {
                continue;
            }

            let total_value_needed = outputs.iter().try_fold(Amount::ZERO, |total, output| {
                total
                    .checked_add(output.output.value - fee_per_user)
                    .context("Output amount overflow after fee application")
            })?;
            let outputs_with_fees = outputs
                .iter()
                .map(|output| TransactionOutput {
                    output: TxOut {
                        script_pubkey: output.output.script_pubkey.clone(),
                        value: output.output.value - fee_per_user,
                    },
                    op_return_data: output.op_return_data.clone(),
                })
                .collect();
            return Ok(TransactionWithFee {
                outputs_with_fees,
                fee,
                total_value_needed,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::{Amount, ScriptBuf, TxOut};

    use super::*;

    fn output(value: Amount) -> TransactionOutput {
        TransactionOutput {
            output: TxOut {
                value,
                script_pubkey: ScriptBuf::new(),
            },
            op_return_data: None,
        }
    }

    #[test]
    fn test_fee_applied_equally_to_outputs() {
        let strategy = WithdrawalFeeStrategy::new();
        let amount = Amount::from_sat(10_000);
        let mut outputs = vec![output(amount), output(amount)];

        let input_count = 2;
        let fee_rate = 2;

        let tx_fee = strategy
            .apply_fee_to_outputs(&mut outputs, input_count, fee_rate)
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

        let mut outputs = vec![output(amount1), output(amount2)];

        let input_count = 2;
        let fee_rate = 10;

        let tx_fee = strategy
            .apply_fee_to_outputs(&mut outputs, input_count, fee_rate)
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

        let mut outputs = vec![output(amount), output(amount)];

        let input_count = 1;
        let fee_rate = 100;

        let tx_fee = strategy
            .apply_fee_to_outputs(&mut outputs, input_count, fee_rate)
            .expect("fee application failed");
        let expected_fee = strategy
            .estimate_fee(input_count, tx_fee.outputs_with_fees.len() as u32, fee_rate)
            .unwrap();

        assert_eq!(tx_fee.outputs_with_fees.len(), 0);
        assert_eq!(tx_fee.total_value_needed, Amount::ZERO);
        assert_eq!(tx_fee.fee, expected_fee);

        let exact_fee = strategy.estimate_fee(1, 1, 1).unwrap();
        let mut boundary = vec![output(exact_fee)];
        assert!(strategy
            .apply_fee_to_outputs(&mut boundary, 1, 1)
            .unwrap()
            .outputs_with_fees
            .is_empty());
    }

    #[test]
    fn test_fee_zero_rate() {
        let strategy = WithdrawalFeeStrategy::new();
        let amount1 = Amount::from_sat(1000);
        let amount2 = Amount::from_sat(2000);
        let mut outputs = vec![output(amount1), output(amount2)];

        let input_count = 1;
        let fee_rate = 0;

        let tx_fee = strategy
            .apply_fee_to_outputs(&mut outputs, input_count, fee_rate)
            .expect("fee application failed");

        assert_eq!(tx_fee.outputs_with_fees.len(), 2);
        assert_eq!(tx_fee.outputs_with_fees[0].output.value, amount1);
        assert_eq!(tx_fee.outputs_with_fees[1].output.value, amount2);
        assert_eq!(tx_fee.fee.to_sat(), 0);

        outputs[0].output.value = Amount::MAX;
        outputs[1].output.value = Amount::ONE_SAT;
        assert!(strategy.apply_fee_to_outputs(&mut outputs, 1, 0).is_err());
    }

    #[test]
    fn test_estimate_fee_multiple_cases() {
        let strategy = WithdrawalFeeStrategy::new();
        for (inputs, outputs, rate, expected) in [
            (2, 3, 10, 4761),
            (1, 4, 15, 5432),
            (1, 1, 20, 5200),
            (3, 5, 8, 5540),
            (2, 7, 50, 30604),
            (1, 0, 10, 2260),
            (5, 10, 25, 28950),
            (1, 2, 1, 294),
        ] {
            let fee = strategy
                .estimate_fee(inputs, outputs, rate)
                .unwrap()
                .to_sat();
            assert_eq!(
                fee, expected,
                "inputs={inputs}, outputs={outputs}, rate={rate}"
            );
            if outputs > 0 {
                assert_eq!(fee % outputs as u64, 0);
            }
        }
    }
}
