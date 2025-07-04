use std::vec;

use bitcoin::{Amount, TxOut};

pub trait FeeStrategy: Send + Sync {
    fn estimate_fee(
        &self,
        input_count: u32,
        output_count: u32,
        fee_rate: u64,
    ) -> anyhow::Result<Amount> {
        // version + locktime
        let base_size = 10_u64;
        // approximate size per input
        let input_size = 148_u64 * u64::from(input_count);
        // approximate size per output +2 (+1 for OP_RETURN, +1 for potential change)
        let output_size = 34_u64 * u64::from(output_count + 2);

        let total_size = base_size + input_size + output_size;
        let fee = fee_rate * total_size;

        Ok(Amount::from_sat(fee))
    }

    fn apply_fee_to_inputs(
        &self,
        outputs: Vec<TxOut>,
        input_count: u32,
        fee_rate: u64,
    ) -> anyhow::Result<(Vec<TxOut>, Amount, Amount)>;
}

pub struct WithdrawalFeeStrategy {}

impl WithdrawalFeeStrategy {
    pub fn new() -> Self {
        Self {}
    }
}

impl FeeStrategy for WithdrawalFeeStrategy {
    fn apply_fee_to_inputs(
        &self,
        mut outputs: Vec<TxOut>,
        input_count: u32,
        fee_rate: u64,
    ) -> anyhow::Result<(Vec<TxOut>, Amount, Amount)> {
        loop {
            let fee = self.estimate_fee(input_count, outputs.len() as u32, fee_rate)?;
            if outputs.is_empty() {
                return Ok((vec![], fee, Amount::ZERO));
            }

            let fee_per_user = Amount::from_sat(fee.to_sat() / outputs.len() as u64);
            let mut new_outputs = vec![];
            let mut new_outputs_with_fee = vec![];
            let mut total_value_needed = Amount::ZERO;

            for output in &outputs {
                if output.value < fee_per_user {
                    continue; // This output can't pay the fee
                }

                let value = output.value - fee_per_user;
                new_outputs_with_fee.push(TxOut {
                    script_pubkey: output.script_pubkey.clone(),
                    value,
                });
                new_outputs.push(TxOut {
                    script_pubkey: output.script_pubkey.clone(),
                    value: output.value,
                });
                total_value_needed += value
            }

            if new_outputs.len() != outputs.len() {
                // Output set has changed, need to re-estimate fee
                outputs = new_outputs;
                continue;
            }

            return Ok((new_outputs_with_fee, fee, total_value_needed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{Amount, ScriptBuf};

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
        let outputs = vec![dummy_output(amount), dummy_output(amount)];

        let input_count = 2;
        let fee_rate = 2;

        let (adjusted_outputs, fee, _) = strategy
            .apply_fee_to_inputs(outputs.clone(), input_count, fee_rate)
            .expect("fee application failed");

        assert_eq!(adjusted_outputs.len(), 2);

        let expected_fee = strategy
            .estimate_fee(input_count, outputs.len() as u32, fee_rate)
            .unwrap();

        let fee_per_output = Amount::from_sat(expected_fee.to_sat() / 2);

        for output in adjusted_outputs {
            assert_eq!(output.value, amount - fee_per_output);
        }

        assert_eq!(fee, expected_fee);
    }

    #[test]
    fn test_small_output_is_removed() {
        let strategy = WithdrawalFeeStrategy::new();
        let amount1 = Amount::from_sat(10_000);
        let amount2 = Amount::from_sat(500);

        let outputs = vec![
            dummy_output(amount1),
            dummy_output(amount2), // this one will be too small to cover fee
        ];

        let input_count = 2;
        let fee_rate = 10;

        let (adjusted_outputs, fee, _) = strategy
            .apply_fee_to_inputs(outputs.clone(), input_count, fee_rate)
            .expect("fee application failed");

        // One output should be removed
        assert_eq!(adjusted_outputs.len(), 1);
        assert!(adjusted_outputs[0].value < amount1);

        // Make sure fee is re-estimated with 1 output
        let expected_fee = strategy.estimate_fee(input_count, 1, fee_rate).unwrap();

        assert_eq!(fee, expected_fee);
    }

    #[test]
    fn test_no_outputs_can_pay_fee() {
        let strategy = WithdrawalFeeStrategy::new();
        let amount = Amount::from_sat(100);

        let outputs = vec![
            dummy_output(amount), // not enough to cover any reasonable fee
            dummy_output(amount),
        ];

        let input_count = 1;
        let fee_rate = 100;

        let (adjusted_outputs, fee, total_value_needed) = strategy
            .apply_fee_to_inputs(outputs.clone(), input_count, fee_rate)
            .expect("fee application failed");
        let expected_fee = strategy
            .estimate_fee(input_count, adjusted_outputs.len() as u32, fee_rate)
            .unwrap();

        assert_eq!(adjusted_outputs.len(), 0);
        assert_eq!(total_value_needed, Amount::ZERO);
        assert_eq!(fee, expected_fee);
    }

    #[test]
    fn test_fee_zero_rate() {
        let strategy = WithdrawalFeeStrategy::new();
        let amount1 = Amount::from_sat(1000);
        let amount2 = Amount::from_sat(2000);
        let outputs = vec![dummy_output(amount1), dummy_output(amount2)];

        let input_count = 1;
        let fee_rate = 0;

        let (adjusted_outputs, fee, _) = strategy
            .apply_fee_to_inputs(outputs.clone(), input_count, fee_rate)
            .expect("fee application failed");

        assert_eq!(adjusted_outputs.len(), 2);
        assert_eq!(adjusted_outputs[0].value, amount1);
        assert_eq!(adjusted_outputs[1].value, amount2);
        assert_eq!(fee.to_sat(), 0);
    }
}
