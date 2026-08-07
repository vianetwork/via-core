#[cfg(test)]
mod tests {
    use std::{
        str::FromStr,
        sync::{
            atomic::{AtomicU32, Ordering},
            Arc,
        },
    };

    use anyhow::Result;
    use bitcoin::{
        absolute, hashes::Hash, policy::MAX_STANDARD_TX_WEIGHT, transaction, Address, Amount,
        Network, OutPoint, ScriptBuf, Transaction, TxOut, Txid, WPubkeyHash,
    };
    use via_btc_client::inscriber::test_utils::{MockBitcoinOps, MockBitcoinOpsConfig};
    use via_verifier_types::transaction::UnsignedBridgeTx;

    use crate::{
        fee::WithdrawalFeeStrategy,
        transaction_builder::TransactionBuilder,
        types::{TransactionBuilderConfig, TransactionOutput},
    };

    const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_W_0";
    const WITHDRAWALS_PER_TRANSACTION: usize = 7;
    static NEXT_REQUEST_ID: AtomicU32 = AtomicU32::new(1);

    fn tx_builder(utxo_values: Vec<Amount>) -> anyhow::Result<TransactionBuilder> {
        let values: Vec<_> = utxo_values.iter().map(|value| value.to_sat()).collect();
        TransactionBuilder::new(Arc::new(MockBitcoinOps::new(MockBitcoinOpsConfig {
            utxos: test_utxos(&values),
            fee_rate: 2,
            ..MockBitcoinOpsConfig::default()
        })))
    }

    async fn create_bridge_tx(
        bridge_address_total_values: Vec<Amount>,
        outputs: Vec<TransactionOutput>,
    ) -> anyhow::Result<Vec<UnsignedBridgeTx>> {
        tx_builder(bridge_address_total_values)?
            .build_transaction_with_op_return(outputs, withdrawal_config())
            .await
    }

    fn withdrawal_config() -> TransactionBuilderConfig {
        TransactionBuilderConfig {
            fee_strategy: Arc::new(WithdrawalFeeStrategy::new()),
            max_tx_weight: MAX_STANDARD_TX_WEIGHT as u64,
            max_output_per_tx: WITHDRAWALS_PER_TRANSACTION,
            op_return_prefix: OP_RETURN_WITHDRAW_PREFIX.to_vec(),
            bridge_address: Address::from_str(
                "bcrt1pxqkh0g270lucjafgngmwv7vtgc8mk9j5y4j8fnrxm77yunuh398qfv8tqp",
            )
            .unwrap()
            .require_network(Network::Regtest)
            .unwrap(),
            default_fee_rate_opt: None,
            default_available_utxos_opt: None,
            op_return_data_input_opt: None,
        }
    }

    fn withdrawal_request(value: Amount) -> TransactionOutput {
        let id = NEXT_REQUEST_ID
            .fetch_add(1, Ordering::Relaxed)
            .to_be_bytes();
        let mut identity = [0; 20];
        identity[..id.len()].copy_from_slice(&id);
        TransactionOutput {
            output: TxOut {
                script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array(identity)),
                value,
            },
            op_return_data: Some(identity[..10].to_vec()),
        }
    }

    fn withdrawal_requests(count: usize, value: Amount) -> Vec<TransactionOutput> {
        (0..count).map(|_| withdrawal_request(value)).collect()
    }

    fn test_utxos(values: &[u64]) -> Vec<(OutPoint, TxOut)> {
        values
            .iter()
            .enumerate()
            .map(|(vout, &value)| {
                (
                    OutPoint::new(Txid::all_zeros(), vout as u32),
                    TxOut {
                        script_pubkey: ScriptBuf::new(),
                        value: Amount::from_sat(value),
                    },
                )
            })
            .collect()
    }

    fn assert_withdrawal_op_return<'a>(
        tx: &Transaction,
        requests: impl IntoIterator<Item = &'a TransactionOutput>,
    ) -> Result<()> {
        let expected = TransactionBuilder::create_op_return_script(
            OP_RETURN_WITHDRAW_PREFIX,
            requests
                .into_iter()
                .map(|request| request.op_return_data.clone().unwrap())
                .collect(),
        )?;
        assert_eq!(
            tx.output
                .iter()
                .find(|output| output.script_pubkey.is_op_return())
                .unwrap()
                .script_pubkey,
            expected
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_conserves_value_and_splits_fee() -> Result<()> {
        for (utxo_values, request_values) in [
            (vec![Amount::from_btc(1.0)?], vec![Amount::from_btc(0.1)?]),
            (
                vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?],
                vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?],
            ),
        ] {
            let requests: Vec<_> = request_values
                .iter()
                .copied()
                .map(withdrawal_request)
                .collect();
            let mut bridge_txs = create_bridge_tx(utxo_values.clone(), requests.clone()).await?;
            assert_eq!(bridge_txs.len(), 1);
            let bridge_tx = bridge_txs.remove(0);

            assert_eq!(
                bridge_tx
                    .tx
                    .output
                    .iter()
                    .map(|out| out.value)
                    .sum::<Amount>()
                    + bridge_tx.fee,
                utxo_values.into_iter().sum()
            );
            let fee_per_user = Amount::from_sat(bridge_tx.fee.to_sat() / requests.len() as u64);
            for (output, request) in bridge_tx.tx.output.iter().zip(&requests) {
                assert_eq!(output.value + fee_per_user, request.output.value);
            }
            assert_eq!(
                bridge_tx.tx.output.len(),
                requests.len() + 1 + usize::from(bridge_tx.change_amount > Amount::ZERO)
            );
            assert_withdrawal_op_return(&bridge_tx.tx, &requests)?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_skips_below_fee_request_in_any_position() -> Result<()> {
        let small_value = Amount::from_sat(20);

        for small_index in [0, 1, 2] {
            let mut requests = vec![
                withdrawal_request(Amount::from_sat(100_000_000)),
                withdrawal_request(Amount::from_sat(99_999_980)),
            ];
            requests.insert(small_index, withdrawal_request(small_value));

            let mut bridge_txs = create_bridge_tx(
                vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?],
                requests.clone(),
            )
            .await?;
            assert_eq!(bridge_txs.len(), 1);
            let bridge_tx = bridge_txs.remove(0);
            assert_eq!(
                bridge_tx
                    .tx
                    .output
                    .iter()
                    .map(|out| out.value)
                    .sum::<Amount>()
                    + bridge_tx.fee,
                Amount::from_btc(2.0)?
            );

            let included: Vec<_> = requests
                .iter()
                .filter(|request| request.output.value > small_value)
                .collect();
            let fee_per_user = Amount::from_sat(bridge_tx.fee.to_sat() / included.len() as u64);
            for (output, request) in bridge_tx.tx.output.iter().zip(&included) {
                assert_eq!(output.value + fee_per_user, request.output.value);
            }

            assert_eq!(bridge_tx.tx.output.len(), 4);
            let bridge_change = bridge_tx.tx.output.last().unwrap();
            assert_eq!(bridge_change.value, small_value);
            assert_withdrawal_op_return(&bridge_tx.tx, included)?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_rejects_invalid_limits() -> Result<()> {
        let mut config = withdrawal_config();
        config.max_output_per_tx = 0;
        let result = tx_builder(vec![])?
            .build_bridge_txs(vec![], vec![], config, 1)
            .await;
        assert!(result.is_err());
        assert!(TransactionBuilder::create_op_return_script(&[0; 80], vec![]).is_ok());
        assert!(TransactionBuilder::create_op_return_script(&[0; 81], vec![]).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_emits_no_transaction_when_all_values_are_below_fee(
    ) -> Result<()> {
        let bridge_address_total_values = vec![Amount::from_btc(1.0)?];
        let users_request_small_value = Amount::from_sat(20);

        let requests = withdrawal_requests(3, users_request_small_value);

        let bridge_txs = create_bridge_tx(bridge_address_total_values, requests).await?;

        assert!(bridge_txs.is_empty());

        Ok(())
    }

    #[test]
    fn test_prepare_build_transaction_selects_inputs_and_charges_outputs() -> anyhow::Result<()> {
        let builder = tx_builder(vec![])?;
        for (utxo_values, output_values) in [
            (&[2_000][..], &[1_000, 1_000][..]),
            (&[1_000, 1_000][..], &[1_500, 500][..]),
        ] {
            let available_utxos = test_utxos(utxo_values);
            let outputs: Vec<_> = output_values
                .iter()
                .map(|value| withdrawal_request(Amount::from_sat(*value)))
                .collect();
            let total_requested = outputs
                .iter()
                .map(|output| output.output.value)
                .sum::<Amount>();
            let (tx_fee, selected_utxos) = builder.prepare_build_transaction(
                outputs.clone(),
                &available_utxos,
                1,
                Arc::new(WithdrawalFeeStrategy::new()),
            )?;

            assert_eq!(tx_fee.total_value_needed + tx_fee.fee, total_requested);
            assert_eq!(tx_fee.outputs_with_fees.len(), outputs.len());
            for (output, requested) in tx_fee.outputs_with_fees.iter().zip(&outputs) {
                assert_eq!(
                    output.output.value
                        + Amount::from_sat(tx_fee.fee.to_sat() / outputs.len() as u64),
                    requested.output.value
                );
            }
            assert_eq!(selected_utxos, available_utxos);
        }

        Ok(())
    }

    #[test]
    fn test_prepare_build_transaction_recomputes_fee_eligibility_per_input() -> anyhow::Result<()> {
        let builder = tx_builder(vec![])?;

        for (utxos, values, fee_rate, expected_inputs, expected_outputs) in [
            (&[1_500][..], &[1_500, 100][..], 1, 1, 1),
            (&[1_000, 147][..], &[1_000, 147][..], 1, 1, 1),
            (&[1_000, 170][..], &[1_000, 170][..], 1, 1, 1),
            (&[1_000, 600][..], &[1_500, 180][..], 1, 2, 1),
            (
                &[8_000, 4_000, 2_000][..],
                &[6_000, 6_000, 2_000][..],
                20,
                2,
                2,
            ),
        ] {
            let outputs: Vec<_> = values
                .iter()
                .map(|value| withdrawal_request(Amount::from_sat(*value)))
                .collect();
            let expected_total = outputs
                .iter()
                .take(expected_outputs)
                .map(|output| output.output.value)
                .sum::<Amount>();
            let (tx_fee, selected_utxos) = builder.prepare_build_transaction(
                outputs,
                &test_utxos(utxos),
                fee_rate,
                Arc::new(WithdrawalFeeStrategy::new()),
            )?;

            assert_eq!(selected_utxos.len(), expected_inputs);
            assert_eq!(tx_fee.outputs_with_fees.len(), expected_outputs);
            assert_eq!(tx_fee.total_value_needed + tx_fee.fee, expected_total);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_bridge_tx_anchors_prevouts_and_rejects_mutations() -> Result<()> {
        let cfg = withdrawal_config();
        let prevout = TxOut {
            value: Amount::from_sat(4_000),
            script_pubkey: cfg.bridge_address.script_pubkey(),
        };
        let parent = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![prevout.clone(), prevout.clone()],
        };
        let parent_txid = parent.compute_txid();
        let utxos = vec![
            (OutPoint::new(parent_txid, 0), prevout.clone()),
            (OutPoint::new(parent_txid, 1), prevout.clone()),
        ];
        let builder =
            TransactionBuilder::new(Arc::new(MockBitcoinOps::new(MockBitcoinOpsConfig {
                transaction: Some(parent),
                ..MockBitcoinOpsConfig::default()
            })))?;
        let request = withdrawal_request(Amount::from_sat(5_000));
        let mut txs = builder
            .build_bridge_txs(utxos.clone(), vec![request.clone()], cfg.clone(), 1)
            .await?;
        let tx = txs.pop().unwrap();
        assert_eq!(tx.utxos.len(), 2);

        assert!(
            builder
                .verify_bridge_tx(&tx, vec![request.clone()], cfg.clone())
                .await?
        );

        let mut overweight = cfg.clone();
        overweight.max_tx_weight = 0;
        assert!(
            !tx_builder(vec![])?
                .verify_bridge_tx(&tx, vec![request.clone()], overweight,)
                .await?
        );

        let mut misaligned = tx.clone();
        misaligned.tx.input[0].previous_output.vout += 1;
        assert!(
            !tx_builder(vec![])?
                .verify_bridge_tx(&misaligned, vec![request.clone()], cfg.clone(),)
                .await?
        );

        let mut underpaid = tx.clone();
        underpaid.tx.output[0].value -= Amount::from_sat(1);
        underpaid.tx.output.last_mut().unwrap().value += Amount::from_sat(1);
        underpaid.txid = underpaid.tx.compute_txid();
        let mut forged_utxos = utxos;
        forged_utxos[0].1.value += Amount::from_sat(1);
        let forged = builder
            .build_bridge_txs(forged_utxos, vec![request.clone()], cfg.clone(), 1)
            .await?
            .pop()
            .unwrap();
        let mut duplicate = tx.clone();
        duplicate.utxos.push(duplicate.utxos[0].clone());
        duplicate.tx.input.push(duplicate.tx.input[0].clone());

        let mut padded = tx.clone();
        for marker in 1..=32 {
            let outpoint = OutPoint::new(Txid::from_byte_array([marker; 32]), 0);
            let mut input = padded.tx.input[0].clone();
            input.previous_output = outpoint;
            padded.tx.input.push(input);
            padded.utxos.push((outpoint, prevout.clone()));
        }
        padded.txid = padded.tx.compute_txid();
        assert!(
            !tx_builder(vec![])?
                .verify_bridge_tx(&padded, vec![request.clone()], cfg.clone())
                .await?
        );

        for invalid in [underpaid, forged, duplicate] {
            assert!(
                !builder
                    .verify_bridge_tx(&invalid, vec![request.clone()], cfg.clone())
                    .await?
            );
        }
        Ok(())
    }

    #[test]
    fn test_prepare_build_transaction_when_all_users_do_not_have_enough_to_cover_tx_fee(
    ) -> anyhow::Result<()> {
        let builder = tx_builder(vec![])?;

        let available_utxos = vec![];

        let outputs = vec![
            withdrawal_request(Amount::from_sat(100)),
            withdrawal_request(Amount::from_sat(100)),
        ];

        let fee_rate = 1;
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let (tx_fee, selected_utxos) = builder.prepare_build_transaction(
            outputs.clone(),
            &available_utxos,
            fee_rate,
            fee_strategy,
        )?;

        assert_eq!(tx_fee.outputs_with_fees.len(), 0);
        assert_ne!(tx_fee.fee, Amount::ZERO);
        assert_eq!(tx_fee.total_value_needed, Amount::ZERO);
        assert!(selected_utxos.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_does_not_chain_zero_change() -> Result<()> {
        let total_withdrawals = 23;
        let requests = withdrawal_requests(total_withdrawals, Amount::from_btc(0.1).unwrap());

        let bridge_txs = create_bridge_tx(
            vec![
                Amount::from_btc(0.7)?,
                Amount::from_btc(0.6)?,
                Amount::from_btc(0.6)?,
                Amount::from_btc(0.4)?,
            ],
            requests.clone(),
        )
        .await?;
        assert_eq!(
            bridge_txs.len(),
            requests.chunks(WITHDRAWALS_PER_TRANSACTION).len()
        );
        assert_eq!(bridge_txs[0].change_amount, Amount::ZERO);
        assert!(bridge_txs[0]
            .tx
            .output
            .last()
            .unwrap()
            .script_pubkey
            .is_op_return());
        assert!(bridge_txs
            .iter()
            .flat_map(|tx| &tx.utxos)
            .all(|(_, output)| output.value > Amount::ZERO));

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_skips_empty_chunks_and_continues_with_valid_withdrawals(
    ) -> Result<()> {
        let invalid_requests =
            withdrawal_requests(WITHDRAWALS_PER_TRANSACTION, Amount::from_sat(100));
        let valid_requests = withdrawal_requests(
            WITHDRAWALS_PER_TRANSACTION * 2,
            Amount::from_btc(0.1).unwrap(),
        );
        let requests = invalid_requests
            .into_iter()
            .chain(valid_requests.iter().cloned())
            .collect();

        let bridge_txs = create_bridge_tx(vec![Amount::from_btc(2.3)?], requests).await?;
        let valid_chunks: Vec<_> = valid_requests.chunks(WITHDRAWALS_PER_TRANSACTION).collect();

        assert_eq!(bridge_txs.len(), valid_chunks.len());
        for (bridge_tx, expected_chunk) in bridge_txs.iter().zip(valid_chunks) {
            assert_eq!(bridge_tx.tx.output.len(), expected_chunk.len() + 2);
            for (output, expected) in bridge_tx.tx.output.iter().zip(expected_chunk) {
                assert_eq!(output.script_pubkey, expected.output.script_pubkey);
            }

            assert_withdrawal_op_return(&bridge_tx.tx, expected_chunk)?;
        }

        let first_tx = &bridge_txs[0];
        assert_eq!(first_tx.utxos.len(), 1);
        assert_eq!(first_tx.utxos[0].1.value, Amount::from_btc(2.3)?);
        assert_eq!(first_tx.utxos[0].1.script_pubkey, ScriptBuf::new());
        let first_change_vout = (first_tx.tx.output.len() - 1) as u32;
        let first_change = first_tx.tx.output[first_change_vout as usize].clone();
        assert_eq!(
            bridge_txs[1].utxos,
            vec![(
                OutPoint {
                    txid: first_tx.txid,
                    vout: first_change_vout,
                },
                first_change,
            )]
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_selects_minimal_inputs_across_chained_txs() -> Result<()> {
        for (utxos, withdrawal_count, expected_inputs) in [
            (vec![0.3, 0.5, 0.2, 1.0], 20, vec![1, 2, 3]),
            (vec![1.5, 0.6], 14, vec![1, 1]),
        ] {
            let utxos = utxos
                .into_iter()
                .map(Amount::from_btc)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let requests = withdrawal_requests(withdrawal_count, Amount::from_btc(0.1).unwrap());
            let bridge_txs = create_bridge_tx(utxos, requests).await?;

            assert_eq!(
                bridge_txs
                    .iter()
                    .map(|tx| tx.tx.input.len())
                    .collect::<Vec<_>>(),
                expected_inputs
            );
        }

        Ok(())
    }
}
