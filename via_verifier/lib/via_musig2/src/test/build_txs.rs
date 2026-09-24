#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use anyhow::Result;
    use async_trait::async_trait;
    use bitcoin::{
        hashes::Hash,
        policy::MAX_STANDARD_TX_WEIGHT,
        secp256k1::{Secp256k1, SecretKey},
        Address, Amount, CompressedPublicKey, Network, NetworkKind, OutPoint, PrivateKey,
        ScriptBuf, Transaction, TxOut, Txid,
    };
    use bitcoincore_rpc::json::GetBlockStatsResult;
    use mockall::{mock, predicate::*};
    use rand::{rngs::OsRng, seq::SliceRandom, thread_rng, RngCore};
    use via_btc_client::{traits::BitcoinOps, types::BitcoinError};
    use via_test_utils::utils::generate_return_data_per_outputs;
    use via_verifier_types::transaction::UnsignedBridgeTx;

    use crate::{
        fee::WithdrawalFeeStrategy,
        transaction_builder::TransactionBuilder,
        types::{TransactionBuilderConfig, TransactionOutput},
    };

    mock! {
        BitcoinOpsService {}
        #[async_trait]
        impl BitcoinOps for BitcoinOpsService {
            async fn fetch_utxos(&self, _address: &Address) -> anyhow::Result<Vec<(OutPoint, TxOut)>, BitcoinError> {
                Ok(vec![])
            }

            async fn get_fee_rate(&self, _target_blocks: u16) -> Result<u64, BitcoinError> {
                Ok(2)
            }

            async fn broadcast_signed_transaction(&self, _tx_hex: &str) -> Result<Txid, BitcoinError> {
                Ok(Txid::ZERO)
            }

            async fn check_tx_confirmation(&self, _txid: &Txid, _min_confirmations: u32) -> Result<bool, BitcoinError> {
                Ok(true)
            }

            async fn fetch_block_height(&self) -> Result<u64, BitcoinError> {
                Ok(100000)
            }

            async fn get_balance(&self, _address: &Address) -> Result<u128, BitcoinError> {
                Ok(100000000)
            }
            fn get_network(&self) -> bitcoin::Network {
                Network::Regtest
            }

            async fn fetch_block(&self, _height: u128) -> Result<bitcoin::Block, BitcoinError> {
                Ok(bitcoin::Block::default())
            }

            async fn get_transaction(&self, _txid: &Txid) -> Result<Transaction, BitcoinError> {
                Ok(Transaction::default())
            }

            async fn fetch_block_by_hash(&self, _hash: &bitcoin::BlockHash) -> Result<bitcoin::Block, BitcoinError> {
                Ok(bitcoin::Block::default())
            }

            async fn get_block_stats(&self, _height: u64) -> Result<GetBlockStatsResult, BitcoinError> {
                Ok(GetBlockStatsResult::default())
            }

            async fn get_fee_history(&self, _start: usize, _end: usize) -> Result<Vec<u64>, BitcoinError> {
                Ok(vec![1])
            }
        }
    }

    const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_W_0";
    const WITHDRAWALS_PER_TRANSACTION: usize = 7;

    fn generate_random_hex_string() -> String {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    }

    fn get_network() -> Network {
        Network::Regtest
    }

    fn get_bridge_address_mock() -> Address {
        let bridge_address =
            Address::from_str("bcrt1pxqkh0g270lucjafgngmwv7vtgc8mk9j5y4j8fnrxm77yunuh398qfv8tqp")
                .unwrap()
                .require_network(get_network())
                .unwrap();
        bridge_address
    }

    fn generate_wallet_address(network: Network) -> Address {
        let secp = Secp256k1::new();
        let mut rng = OsRng;

        let secret_key = SecretKey::new(&mut rng);

        let private_key = PrivateKey {
            compressed: true,
            network: NetworkKind::Test,
            inner: secret_key,
        };

        // let public_key = private_key.public_key(&secp);
        let public_key = CompressedPublicKey::from_private_key(&secp, &private_key).unwrap();
        let address = Address::p2wpkh(&public_key, network);
        address
    }

    fn create_btc_client_mock(utxo_values: Vec<Amount>) -> Arc<MockBitcoinOpsService> {
        let mut mock_ops = MockBitcoinOpsService::new();
        mock_ops.expect_fetch_utxos().returning(move |_| {
            let mut utxos = vec![];
            for value in &utxo_values {
                let txid = Txid::from_str(&generate_random_hex_string()).unwrap();
                let outpoint = OutPoint::new(txid, 0);
                let txout = TxOut {
                    value: value.clone(),
                    script_pubkey: ScriptBuf::new(),
                };
                utxos.push((outpoint, txout))
            }
            Ok(utxos)
        });
        mock_ops.expect_get_fee_rate().returning(|_| Ok(2));

        let btc_client = Arc::new(mock_ops);
        btc_client
    }

    fn create_tx_builder_mock(
        btc_client_mock: Option<Arc<MockBitcoinOpsService>>,
    ) -> anyhow::Result<TransactionBuilder> {
        let btc_client = match btc_client_mock {
            Some(btc_client) => btc_client,
            None => create_btc_client_mock(vec![]),
        };

        TransactionBuilder::new(btc_client)
    }

    async fn create_bridge_tx(
        bridge_address_total_values: Vec<Amount>,
        outputs: Vec<TransactionOutput>,
    ) -> anyhow::Result<Vec<UnsignedBridgeTx>> {
        let btc_client = create_btc_client_mock(bridge_address_total_values.clone());
        let builder = create_tx_builder_mock(Some(btc_client))?;

        let config = TransactionBuilderConfig {
            fee_strategy: Arc::new(WithdrawalFeeStrategy::new()),
            max_tx_weight: MAX_STANDARD_TX_WEIGHT as u64,
            max_output_per_tx: WITHDRAWALS_PER_TRANSACTION,
            op_return_prefix: OP_RETURN_WITHDRAW_PREFIX.to_vec(),
            bridge_address: get_bridge_address_mock(),
            default_fee_rate_opt: None,
            default_available_utxos_opt: None,
            op_return_data_input_opt: None,
        };

        let bridge_txs = builder
            .build_transaction_with_op_return(outputs.clone(), config, &[])
            .await?;

        Ok(bridge_txs)
    }

    #[tokio::test]
    async fn test_withdrawal_builder_one_user_and_value_greater_than_tx_fee() -> Result<()> {
        let bridge_address_total_value = vec![Amount::from_btc(1.0)?];
        let user_requested_value = vec![Amount::from_btc(0.1)?];

        let requests = vec![TransactionOutput {
            output: TxOut {
                script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                value: Amount::from_btc(0.1)?,
            },
            op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
        }];

        let bridge_txs =
            create_bridge_tx(bridge_address_total_value.clone(), requests.clone()).await?;

        assert_eq!(bridge_txs.len(), 1);

        let bridge_tx = bridge_txs[0].clone();

        let total_value_include_fee = bridge_tx
            .tx
            .output
            .iter()
            .map(|out| out.value)
            .sum::<Amount>()
            + bridge_tx.fee.clone();

        let user_output = bridge_tx.tx.output[0].clone();

        // The total outputs with fee should be equal to the total bridge address before transaction.
        assert_eq!(total_value_include_fee, bridge_address_total_value[0]);

        // The user should pay all the fee
        assert_eq!(user_output.value + bridge_tx.fee, user_requested_value[0]);

        // Verify OP_RETURN output
        let op_return_output = bridge_tx
            .tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())
            .expect("OP_RETURN output not found");

        // Check if the prefix is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .windows(OP_RETURN_WITHDRAW_PREFIX.len())
            .any(|window| window == OP_RETURN_WITHDRAW_PREFIX));

        let expected_op_return_data = TransactionBuilder::create_op_return_script(
            OP_RETURN_WITHDRAW_PREFIX,
            vec![requests[0].op_return_data.clone().unwrap()],
        )?;

        // Check if the reveal tx is included
        assert_eq!(op_return_output.script_pubkey, expected_op_return_data);

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_many_users_and_value_greater_than_tx_fee() -> Result<()> {
        let bridge_address_total_values = vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?];

        let requests = vec![
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_btc(1.0)?,
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_btc(1.0)?,
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
        ];

        let bridge_txs =
            create_bridge_tx(bridge_address_total_values.clone(), requests.clone()).await?;

        let bridge_tx = bridge_txs[0].clone();

        let total_bridge_value_before = bridge_address_total_values
            .clone()
            .iter()
            .map(|value| *value)
            .sum::<Amount>();

        let total_value_include_fee = bridge_tx
            .tx
            .output
            .iter()
            .map(|out| out.value)
            .sum::<Amount>()
            + bridge_tx.fee.clone();

        // The total outputs with fee should be equal to the total bridge address before transaction.
        assert_eq!(total_value_include_fee, total_bridge_value_before);

        let fee_per_user = Amount::from_sat(bridge_tx.fee.to_sat() / 2);

        for (i, w) in requests.iter().enumerate() {
            let user1_output = bridge_tx.tx.output[i].clone();

            // The user should pay 1/2 total fee (there are 2 users).
            assert_eq!(user1_output.value + fee_per_user, w.output.value);
        }

        // Expected outputs [user1, user2 and the OP_RETURN], there is no "change" output.
        assert_eq!(bridge_tx.tx.output.len(), 3);

        // The last output should be the OP_RETURN
        let op_return_output = bridge_tx.tx.output.last().unwrap();

        // Check if the prefix is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .windows(OP_RETURN_WITHDRAW_PREFIX.len())
            .any(|window| window == OP_RETURN_WITHDRAW_PREFIX));

        let expected_op_return_data = TransactionBuilder::create_op_return_script(
            OP_RETURN_WITHDRAW_PREFIX,
            vec![
                requests[0].op_return_data.clone().unwrap(),
                requests[1].op_return_data.clone().unwrap(),
            ],
        )?;

        // Check if the reveal tx is included
        assert_eq!(op_return_output.script_pubkey, expected_op_return_data);

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_many_users_but_one_user_value_less_than_tx_fee() -> Result<()>
    {
        let bridge_address_total_values = vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?];
        // This user requested a small value, it should be ignored when process withdrawal
        let users_request_small_value = Amount::from_sat(20);

        let requests = vec![
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(100000000),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(99999980),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: users_request_small_value,
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
        ];

        let bridge_txs =
            create_bridge_tx(bridge_address_total_values.clone(), requests.clone()).await?;

        let bridge_tx = bridge_txs[0].clone();

        let total_bridge_value_before = bridge_address_total_values
            .clone()
            .iter()
            .map(|value| *value)
            .sum::<Amount>();

        let total_output_value_include_fee = bridge_tx
            .tx
            .output
            .iter()
            .map(|out| out.value)
            .sum::<Amount>()
            + bridge_tx.fee.clone();

        // The total outputs with fee should be equal to the total bridge address before transaction.
        assert_eq!(total_output_value_include_fee, total_bridge_value_before);

        // Divide the fee per 2 because the 3rd user was ignored due to low value.
        let fee_per_user = Amount::from_sat(bridge_tx.fee.to_sat() / 2);

        for (i, w) in requests.iter().enumerate() {
            if w.output.value < fee_per_user {
                continue;
            }
            let user1_output = bridge_tx.tx.output[i].clone();

            // The user should pay 1/2 total fee (there are 2 users).
            assert_eq!(user1_output.value + fee_per_user, w.output.value.clone());
        }

        // The user 3 request amount should sent back to the bridge address
        let bridge_address_change_output = bridge_tx.tx.output.last().unwrap();
        assert_eq!(
            bridge_address_change_output.value,
            users_request_small_value
        );

        // Expected outputs [user1, user2 and the OP_RETURN, change], there is no "user3" output.
        assert_eq!(bridge_tx.tx.output.len(), 4);

        // The len-1 output should be the OP_RETURN
        let op_return_output = bridge_tx
            .tx
            .output
            .get(bridge_tx.tx.output.len() - 2)
            .unwrap();

        // Check if the prefix is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .windows(OP_RETURN_WITHDRAW_PREFIX.len())
            .any(|window| window == OP_RETURN_WITHDRAW_PREFIX));

        // Don't include the requests[2].op_return_data because the withdrawal will not be included as it doesn't fee
        let expected_op_return_data = TransactionBuilder::create_op_return_script(
            OP_RETURN_WITHDRAW_PREFIX,
            vec![
                requests[0].op_return_data.clone().unwrap(),
                requests[1].op_return_data.clone().unwrap(),
            ],
        )?;

        // Check if the reveal tx is included
        assert_eq!(op_return_output.script_pubkey, expected_op_return_data);

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_many_users_but_one_user_value_less_than_tx_fee_when_first(
    ) -> Result<()> {
        let bridge_address_total_values = vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?];
        // This user requested a small value, it should be ignored when process withdrawal
        let users_request_small_value = Amount::from_sat(20);

        let requests = vec![
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: users_request_small_value,
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(100000000),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(99999980),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
        ];

        let bridge_txs =
            create_bridge_tx(bridge_address_total_values.clone(), requests.clone()).await?;

        let bridge_tx = bridge_txs[0].clone();

        let total_bridge_value_before = bridge_address_total_values
            .clone()
            .iter()
            .map(|value| *value)
            .sum::<Amount>();

        let total_output_value_include_fee = bridge_tx
            .tx
            .output
            .iter()
            .map(|out| out.value)
            .sum::<Amount>()
            + bridge_tx.fee.clone();

        // The total outputs with fee should be equal to the total bridge address before transaction.
        assert_eq!(total_output_value_include_fee, total_bridge_value_before);

        // Divide the fee per 2 because the 3rd user was ignored due to low value.
        let fee_per_user = Amount::from_sat(bridge_tx.fee.to_sat() / 2);

        let mut i = 0;
        for w in requests.clone() {
            if w.output.value < fee_per_user {
                continue;
            }
            let user1_output = bridge_tx.tx.output[i].clone();

            // The user should pay 1/2 total fee (there are 2 users).
            assert_eq!(user1_output.value + fee_per_user, w.output.value.clone());
            i += 1;
        }

        // The user 3 request amount should sent back to the bridge address
        let bridge_address_change_output = bridge_tx.tx.output.last().unwrap();
        assert_eq!(
            bridge_address_change_output.value,
            users_request_small_value
        );

        // Expected outputs [user1, user2 and the OP_RETURN, change], there is no "user3" output.
        assert_eq!(bridge_tx.tx.output.len(), 4);

        // The len-1 output should be the OP_RETURN
        let op_return_output = bridge_tx
            .tx
            .output
            .get(bridge_tx.tx.output.len() - 2)
            .unwrap();

        // Check if the prefix is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .windows(OP_RETURN_WITHDRAW_PREFIX.len())
            .any(|window| window == OP_RETURN_WITHDRAW_PREFIX));

        // Don't include the requests[0].op_return_data because the withdrawal will not be included as it doesn't fee
        let expected_op_return_data = TransactionBuilder::create_op_return_script(
            OP_RETURN_WITHDRAW_PREFIX,
            vec![
                requests[1].op_return_data.clone().unwrap(),
                requests[2].op_return_data.clone().unwrap(),
            ],
        )?;

        // Check if the reveal tx is included
        assert_eq!(op_return_output.script_pubkey, expected_op_return_data);

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_many_users_but_all_value_less_than_tx_fee() -> Result<()> {
        let bridge_address_total_values = vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?];
        // This user requested a small value, it should be ignored when process withdrawal
        let users_request_small_value = Amount::from_sat(20);

        let requests = vec![
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: users_request_small_value,
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: users_request_small_value,
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: users_request_small_value,
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
        ];

        let bridge_txs =
            create_bridge_tx(bridge_address_total_values.clone(), requests.clone()).await?;

        let bridge_tx = bridge_txs[0].clone();

        let total_output_value_include_fee = bridge_tx
            .tx
            .output
            .iter()
            .map(|out| out.value)
            .sum::<Amount>()
            + bridge_tx.fee.clone();

        // The total outputs with fee should be equal to the first utxo value bridge address before transaction.
        assert_eq!(
            total_output_value_include_fee,
            bridge_address_total_values[0]
        );

        // The user 3 request amount should sent back to the bridge address
        let bridge_address_change_output = bridge_tx.tx.output.last().unwrap();
        assert_eq!(
            bridge_address_change_output.value,
            total_output_value_include_fee - bridge_tx.fee.clone()
        );

        // Expected outputs [OP_RETURN, change], there is no "user1, user2, user3" output.
        assert_eq!(bridge_tx.tx.output.len(), 2);

        // The first output should be the OP_RETURN
        let op_return_output = bridge_tx.tx.output.first().unwrap();

        // Check if the prefix is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .windows(OP_RETURN_WITHDRAW_PREFIX.len())
            .any(|window| window == OP_RETURN_WITHDRAW_PREFIX));

        // Don't include the requests because the withdrawal will not be included as it doesn't fee
        let expected_op_return_data =
            TransactionBuilder::create_op_return_script(OP_RETURN_WITHDRAW_PREFIX, vec![])?;

        // Check if the reveal tx is included
        assert_eq!(op_return_output.script_pubkey, expected_op_return_data);

        Ok(())
    }

    #[tokio::test]
    async fn test_prepare_build_transaction_when_outputs_requires_all_input_value(
    ) -> anyhow::Result<()> {
        let builder = create_tx_builder_mock(None)?;

        let available_utxos = vec![(
            OutPoint {
                txid: Txid::all_zeros(),
                vout: 0,
            },
            TxOut {
                script_pubkey: ScriptBuf::new(),
                value: Amount::from_sat(2000),
            },
        )];

        let outputs = vec![
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(1000),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(1000),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
        ];

        let fee_rate = 1;
        let total_requested = outputs
            .iter()
            .map(|output| output.output.value)
            .sum::<Amount>();
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let (tx_fee, adjusted_selected_utxos) = builder
            .prepare_build_transaction(outputs.clone(), &available_utxos, fee_rate, fee_strategy)
            .await?;
        assert_eq!(tx_fee.total_value_needed + tx_fee.fee, total_requested);
        assert_eq!(tx_fee.outputs_with_fees.len(), outputs.len());

        // Check if the fee is applied to the outputs
        for (i, output) in tx_fee.outputs_with_fees.iter().enumerate() {
            assert_eq!(
                output.output.value + Amount::from_sat(tx_fee.fee.to_sat() / outputs.len() as u64),
                outputs[i].output.value
            );
        }
        assert_eq!(adjusted_selected_utxos, available_utxos);

        Ok(())
    }

    #[tokio::test]
    async fn test_prepare_build_transaction_when_outputs_requires_multiple_inputs_value(
    ) -> anyhow::Result<()> {
        let builder = create_tx_builder_mock(None)?;

        let available_utxos = vec![
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    script_pubkey: ScriptBuf::new(),
                    value: Amount::from_sat(1000),
                },
            ),
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    script_pubkey: ScriptBuf::new(),
                    value: Amount::from_sat(1000),
                },
            ),
        ];

        let outputs = vec![
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(1500),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(500),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
        ];

        let fee_rate = 1;
        let total_requested = outputs
            .iter()
            .map(|output| output.output.value)
            .sum::<Amount>();
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let (tx_fee, adjusted_selected_utxos) = builder
            .prepare_build_transaction(outputs.clone(), &available_utxos, fee_rate, fee_strategy)
            .await?;
        assert_eq!(tx_fee.total_value_needed + tx_fee.fee, total_requested);
        assert_eq!(tx_fee.outputs_with_fees.len(), outputs.len());

        // Check if the fee is applied to the outputs
        for (i, output) in tx_fee.outputs_with_fees.iter().enumerate() {
            assert_eq!(
                output.output.value + Amount::from_sat(tx_fee.fee.to_sat() / outputs.len() as u64),
                outputs[i].output.value
            );
        }
        assert_eq!(adjusted_selected_utxos, available_utxos);

        Ok(())
    }

    #[tokio::test]
    async fn test_prepare_build_transaction_when_user_did_not_have_enough_to_cover_tx_fee(
    ) -> anyhow::Result<()> {
        let builder = create_tx_builder_mock(None)?;

        let available_utxos = vec![
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    script_pubkey: ScriptBuf::new(),
                    value: Amount::from_sat(1000),
                },
            ),
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    script_pubkey: ScriptBuf::new(),
                    value: Amount::from_sat(1000),
                },
            ),
        ];

        let user1_value = Amount::from_sat(1500);
        let user1_script_pubkey =
            Address::from_str("bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56")?
                .assume_checked()
                .script_pubkey();

        let outputs = vec![
            TransactionOutput {
                output: TxOut {
                    script_pubkey: user1_script_pubkey.clone(),
                    value: user1_value,
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(100),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
        ];

        let fee_rate = 1;
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let (tx_fee, _) = builder
            .prepare_build_transaction(outputs.clone(), &available_utxos, fee_rate, fee_strategy)
            .await?;
        // The user 2 is not included because his withdrawal value can not cover the fee
        assert_eq!(tx_fee.outputs_with_fees.len(), 1);
        assert_eq!(
            tx_fee.outputs_with_fees[0].output.value.clone() + tx_fee.fee,
            user1_value
        );
        assert_eq!(
            tx_fee.outputs_with_fees[0].output.script_pubkey,
            user1_script_pubkey
        );
        assert_eq!(tx_fee.total_value_needed + tx_fee.fee, user1_value);

        Ok(())
    }

    #[tokio::test]
    async fn test_prepare_build_transaction_when_all_users_do_not_have_enough_to_cover_tx_fee(
    ) -> anyhow::Result<()> {
        let builder = create_tx_builder_mock(None)?;

        let available_utxos = vec![
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    script_pubkey: ScriptBuf::new(),
                    value: Amount::from_sat(1000),
                },
            ),
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    script_pubkey: ScriptBuf::new(),
                    value: Amount::from_sat(1000),
                },
            ),
        ];

        let outputs = vec![
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(100),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
            TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(100),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            },
        ];

        let fee_rate = 1;
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let (tx_fee, selected_utxos) = builder
            .prepare_build_transaction(outputs.clone(), &available_utxos, fee_rate, fee_strategy)
            .await?;

        assert_eq!(tx_fee.outputs_with_fees.len(), 0);
        assert_ne!(tx_fee.fee, Amount::ZERO);
        assert_eq!(tx_fee.total_value_needed, Amount::ZERO);
        assert_eq!(selected_utxos, vec![available_utxos[0].clone()]);

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_multiple_bridge_tx_with_all_valid_withdrawals() -> Result<()> {
        let bridge_address_total_value = vec![Amount::from_btc(2.3)?];

        let total_withdrawals = 23;
        let expected_bridge_txs =
            (total_withdrawals as f64 / WITHDRAWALS_PER_TRANSACTION as f64).ceil() as usize;
        let requests: Vec<TransactionOutput> = (0..total_withdrawals)
            .map(|_| TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_btc(0.1).unwrap(),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            })
            .collect();

        let bridge_txs =
            create_bridge_tx(bridge_address_total_value.clone(), requests.clone()).await?;

        assert_eq!(bridge_txs.len(), expected_bridge_txs);

        let mut i = 0;

        let requests_chunks = requests
            .clone()
            .chunks(WITHDRAWALS_PER_TRANSACTION)
            .map(|c| c.to_vec())
            .collect::<Vec<_>>();

        for (index, bridge_tx) in bridge_txs.iter().enumerate() {
            let len = bridge_tx.tx.output.len();

            // The last bridge tx contains 2 withdrawals and just the OP_return, there is no change.
            if index > 2 {
                assert_eq!(len - 1, total_withdrawals % WITHDRAWALS_PER_TRANSACTION);
            } else {
                assert_eq!(len - 2, WITHDRAWALS_PER_TRANSACTION);
            }
            // Check if the of the outputs match the requests (ignore OP_RETURN and change)
            for j in 0..(len - 2) {
                assert_eq!(
                    bridge_tx.tx.output[j].script_pubkey,
                    requests[i].output.script_pubkey
                );
                i += 1;
            }

            // Verify OP_RETURN output
            let op_return_output = bridge_tx
                .tx
                .output
                .iter()
                .find(|output| output.script_pubkey.is_op_return())
                .expect("OP_RETURN output not found");

            // Check if the prefix is included
            assert!(op_return_output
                .script_pubkey
                .as_bytes()
                .windows(OP_RETURN_WITHDRAW_PREFIX.len())
                .any(|window| window == OP_RETURN_WITHDRAW_PREFIX));

            let expected_op_return_data = TransactionBuilder::create_op_return_script(
                OP_RETURN_WITHDRAW_PREFIX,
                requests_chunks[index]
                    .iter()
                    .map(|req| req.op_return_data.clone().unwrap())
                    .collect::<Vec<Vec<u8>>>(),
            )?;
            // Check if the reveal tx is included
            assert_eq!(op_return_output.script_pubkey, expected_op_return_data);
        }

        let mut last_change = None;
        // Check if the transactions are chained, the input should be the change of the next bridge_tx
        for bridge_tx in bridge_txs {
            if last_change.is_none() {
                let len = bridge_tx.tx.output.len();
                last_change = Some(bridge_tx.tx.output[len - 1].clone());
                continue;
            }

            let len = bridge_tx.utxos.len();
            assert_eq!(
                last_change.clone().unwrap().value,
                bridge_tx.utxos[len - 1].1.value
            );
            assert_eq!(
                last_change.clone().unwrap().script_pubkey,
                bridge_tx.utxos[len - 1].1.script_pubkey
            );
            let len = bridge_tx.tx.output.len();
            last_change = Some(bridge_tx.tx.output[len - 1].clone());
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_multiple_bridge_tx_with_some_valid_withdrawals() -> Result<()> {
        let bridge_address_total_value = vec![Amount::from_btc(2.3)?];

        let total_valid_withdrawals = 20;
        let total_invalid_withdrawals = 3;
        let total_withdrawals = total_valid_withdrawals + total_invalid_withdrawals;
        let expected_bridge_txs =
            (total_withdrawals as f64 / WITHDRAWALS_PER_TRANSACTION as f64).ceil() as usize;

        let valid_requests: Vec<TransactionOutput> = (0..total_valid_withdrawals)
            .map(|_| TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_btc(0.1).unwrap(),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            })
            .collect();

        let invalid_requests: Vec<TransactionOutput> = (0..total_invalid_withdrawals)
            .map(|_| TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_sat(100),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            })
            .collect();

        let mut requests = Vec::new();

        requests.extend(valid_requests);
        requests.extend(invalid_requests);

        let mut rng = thread_rng();
        requests.shuffle(&mut rng);

        let bridge_txs =
            create_bridge_tx(bridge_address_total_value.clone(), requests.clone()).await?;

        assert_eq!(bridge_txs.len(), expected_bridge_txs);

        let mut i = 0;

        let requests_chunks = requests
            .clone()
            .chunks(WITHDRAWALS_PER_TRANSACTION)
            .map(|c| c.to_vec())
            .collect::<Vec<_>>();

        let mut total_outputs = 0;
        for (index, bridge_tx) in bridge_txs.iter().enumerate() {
            let len = bridge_tx.tx.output.len();

            total_outputs += len - 2;

            // Check if the of the outputs match the requests (ignore OP_RETURN and change)
            for j in 0..(len - 2) {
                // Ignore the transactions that can not cover the tx fee as they are not included
                if requests[i].output.value < bridge_tx.fee {
                    continue;
                }

                assert_eq!(
                    bridge_tx.tx.output[j].script_pubkey,
                    requests[i].output.script_pubkey
                );
                i += 1;
            }

            // Verify OP_RETURN output
            let op_return_output = bridge_tx
                .tx
                .output
                .iter()
                .find(|output| output.script_pubkey.is_op_return())
                .expect("OP_RETURN output not found");

            // Check if the prefix is included
            assert!(op_return_output
                .script_pubkey
                .as_bytes()
                .windows(OP_RETURN_WITHDRAW_PREFIX.len())
                .any(|window| window == OP_RETURN_WITHDRAW_PREFIX));

            let expected_op_return_data = TransactionBuilder::create_op_return_script(
                OP_RETURN_WITHDRAW_PREFIX,
                // Filter out the invalid requests.
                requests_chunks[index]
                    .iter()
                    .filter(|req| req.output.value > bridge_tx.fee)
                    .map(|req| req.op_return_data.clone().unwrap())
                    .collect::<Vec<Vec<u8>>>(),
            )?;
            // Check if the reveal tx is included
            assert_eq!(op_return_output.script_pubkey, expected_op_return_data);
        }

        assert_eq!(total_outputs, total_valid_withdrawals);

        let mut last_change = None;
        // Check if the transactions are chained, the input should be the change of the next bridge_tx
        for bridge_tx in bridge_txs {
            if last_change.is_none() {
                let len = bridge_tx.tx.output.len();
                last_change = Some(bridge_tx.tx.output[len - 1].clone());
                continue;
            }

            let len = bridge_tx.utxos.len();
            assert_eq!(
                last_change.clone().unwrap().value,
                bridge_tx.utxos[len - 1].1.value
            );
            assert_eq!(
                last_change.clone().unwrap().script_pubkey,
                bridge_tx.utxos[len - 1].1.script_pubkey
            );
            let len = bridge_tx.tx.output.len();
            last_change = Some(bridge_tx.tx.output[len - 1].clone());
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_multiple_bridge_tx_with_multiple_inputs() -> Result<()> {
        let bridge_address_total_value = vec![
            Amount::from_btc(0.3)?,
            Amount::from_btc(0.5)?,
            Amount::from_btc(0.2)?,
            Amount::from_btc(1.0)?,
        ];

        let total_withdrawals = 20;
        let expected_bridge_txs =
            (total_withdrawals as f64 / WITHDRAWALS_PER_TRANSACTION as f64).ceil() as usize;
        let requests: Vec<TransactionOutput> = (0..total_withdrawals)
            .map(|_| TransactionOutput {
                output: TxOut {
                    script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                    value: Amount::from_btc(0.1).unwrap(),
                },
                op_return_data: Some(generate_return_data_per_outputs(1)[0].clone()),
            })
            .collect();

        let bridge_txs =
            create_bridge_tx(bridge_address_total_value.clone(), requests.clone()).await?;

        assert_eq!(bridge_txs.len(), expected_bridge_txs);

        // Check if the inputs
        assert_eq!(bridge_txs[0].tx.input.len(), 1);
        assert_eq!(bridge_txs[1].tx.input.len(), 2);
        assert_eq!(bridge_txs[2].tx.input.len(), 3);

        Ok(())
    }

    fn fixed_fixture(
        value: u64,
        gross: u64,
    ) -> (
        TransactionBuilder,
        TransactionBuilderConfig,
        Transaction,
        Vec<TransactionOutput>,
    ) {
        let bridge = get_bridge_address_mock();
        let parent = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(value),
                script_pubkey: bridge.script_pubkey(),
            }],
        };
        let config = TransactionBuilderConfig::withdrawal(bridge.clone());
        let outputs = vec![TransactionOutput {
            output: TxOut {
                value: Amount::from_sat(gross),
                script_pubkey: bridge.script_pubkey(),
            },
            op_return_data: Some(vec![1; 10]),
        }];
        (
            create_tx_builder_mock(None).unwrap(),
            config,
            parent,
            outputs,
        )
    }

    #[tokio::test]
    async fn held_inputs_are_excluded_before_coin_selection() -> Result<()> {
        let (builder, mut config, parent, outputs) = fixed_fixture(20_000, 10_000);
        let held = (
            OutPoint::new(parent.compute_txid(), 0),
            parent.output[0].clone(),
        );
        let mut available_parent = parent.clone();
        available_parent.output[0].value = Amount::from_sat(15_000);
        let available = (
            OutPoint::new(available_parent.compute_txid(), 0),
            available_parent.output[0].clone(),
        );
        config.default_available_utxos_opt = Some(vec![held.clone(), available.clone()]);
        config.default_fee_rate_opt = Some(1);

        let transactions = builder
            .build_transaction_with_op_return(outputs.clone(), config.clone(), &[held.0])
            .await?;
        assert_eq!(transactions[0].utxos, vec![available.clone()]);
        assert!(builder
            .build_transaction_with_op_return(outputs, config, &[held.0, available.0],)
            .await
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn fixed_construction_preserves_producer_and_zero_net_no_change() -> Result<()> {
        for (value, gross, net, change) in [(20_000, 10_000, 9_746, 10_000), (254, 254, 0, 0)] {
            let (builder, config, parent, outputs) = fixed_fixture(value, gross);
            let inputs = vec![(
                OutPoint::new(parent.compute_txid(), 0),
                parent.output[0].clone(),
            )];
            let fixed = builder.build_fixed_bridge_tx(&inputs, outputs.clone(), &config, 1)?;
            let produced = builder
                .build_bridge_txs(inputs, outputs.clone(), config.clone(), 1)
                .await?;
            assert_eq!(
                bitcoin::consensus::serialize(&fixed.tx),
                bitcoin::consensus::serialize(&produced[0].tx)
            );
            assert_eq!(fixed, produced[0]);
            assert_eq!(fixed.tx.output[0].value.to_sat(), net);
            assert_eq!(fixed.fee.to_sat(), 254);
            assert_eq!(fixed.change_amount.to_sat(), change);
            assert_eq!(fixed.tx.output.len(), if change == 0 { 2 } else { 3 });
            builder.verify_fixed_bridge_tx(&fixed, outputs, &config, &[parent])?;
        }
        Ok(())
    }

    #[test]
    fn fixed_authorization_rejects_mutated_transaction_and_derived_fields() -> Result<()> {
        let (builder, config, parent, outputs) = fixed_fixture(20_000, 10_000);
        let inputs = vec![(
            OutPoint::new(parent.compute_txid(), 0),
            parent.output[0].clone(),
        )];
        let fixed = builder.build_fixed_bridge_tx(&inputs, outputs.clone(), &config, 1)?;
        builder.verify_fixed_bridge_tx(&fixed, outputs.clone(), &config, &[parent.clone()])?;
        let mutations: Vec<Box<dyn Fn(&mut UnsignedBridgeTx)>> = vec![
            Box::new(|tx| tx.tx.output[0].value = Amount::from_sat(9_747)),
            Box::new(|tx| tx.tx.output[0].script_pubkey = ScriptBuf::new()),
            Box::new(|tx| tx.tx.output[1].script_pubkey = ScriptBuf::new_op_return([2; 10])),
            Box::new(|tx| tx.tx.output[2].value = Amount::from_sat(9_999)),
            Box::new(|tx| tx.tx.input[0].sequence = bitcoin::Sequence::MAX),
            Box::new(|tx| tx.tx.input[0].script_sig = ScriptBuf::new_op_return([1])),
            Box::new(|tx| tx.utxos[0].1.value = Amount::from_sat(20_001)),
            Box::new(|tx| tx.fee = Amount::from_sat(255)),
            Box::new(|tx| tx.fee_rate = 2),
            Box::new(|tx| tx.change_amount = Amount::from_sat(9_999)),
            Box::new(|tx| tx.txid = Txid::all_zeros()),
        ];
        for mutate in mutations {
            let mut changed = fixed.clone();
            mutate(&mut changed);
            assert!(builder
                .verify_fixed_bridge_tx(&changed, outputs.clone(), &config, &[parent.clone()])
                .is_err());
        }
        let mut fake_parent = parent.clone();
        fake_parent.output[0].value = Amount::from_sat(20_001);
        assert!(builder
            .verify_fixed_bridge_tx(&fixed, outputs, &config, &[fake_parent])
            .is_err());
        Ok(())
    }

    #[test]
    fn fixed_inputs_require_exact_sufficient_prefix_and_aligned_sighashes() -> Result<()> {
        let (builder, config, parent, outputs) = fixed_fixture(20_000, 10_000);
        let input = (
            OutPoint::new(parent.compute_txid(), 0),
            parent.output[0].clone(),
        );
        let mut trailing = input.clone();
        trailing.0.vout = 1;
        assert!(builder
            .build_fixed_bridge_tx(
                &[input.clone(), trailing.clone()],
                outputs.clone(),
                &config,
                1
            )
            .is_err());
        assert!(builder
            .build_fixed_bridge_tx(&[input.clone(), input.clone()], outputs.clone(), &config, 1)
            .is_err());
        let mut first = input.clone();
        first.1.value = Amount::from_sat(5_000);
        trailing.1.value = Amount::from_sat(6_000);
        let fixed = builder.build_fixed_bridge_tx(&[first, trailing], outputs, &config, 1)?;
        assert_eq!(fixed.change_amount.to_sat(), 1_000);
        assert_eq!(builder.get_tr_sighashes(&fixed)?.len(), 2);
        let mut changed = fixed.clone();
        changed.utxos.reverse();
        assert!(builder.get_tr_sighashes(&changed).is_err());
        changed.utxos.clear();
        assert!(builder.get_tr_sighashes(&changed).is_err());
        let (_, _, _, small) = fixed_fixture(20_000, 253);
        assert!(builder
            .build_fixed_bridge_tx(&[input], small, &config, 1)
            .is_err());
        Ok(())
    }

    #[test]
    fn observed_payment_accepts_governance_witness_without_local_round_and_rejects_underpayment(
    ) -> Result<()> {
        use via_verifier_types::{
            withdrawal::WithdrawalRequest,
            withdrawal_observation::{ObservedWithdrawal, WithdrawalObservation},
        };
        let (builder, config, parent, outputs) = fixed_fixture(254, 254);
        let inputs = vec![(
            OutPoint::new(parent.compute_txid(), 0),
            parent.output[0].clone(),
        )];
        let fixed = builder.build_fixed_bridge_tx(&inputs, outputs, &config, 1)?;
        let requests = vec![WithdrawalRequest {
            id: hex::encode([1; 10]),
            receiver: config.bridge_address.clone(),
            amount: Amount::from_sat(254),
            l2_sender: Default::default(),
            l2_tx_hash: Default::default(),
            l2_tx_log_index: 0,
        }];
        let mut observation = WithdrawalObservation {
            transaction: fixed.tx,
            prevouts: inputs,
            withdrawals: vec![ObservedWithdrawal {
                vout: 0,
                reference: requests[0].id.clone(),
                script_pubkey: config.bridge_address.script_pubkey(),
                amount: Amount::ZERO,
            }],
            inclusion: None,
        };
        observation.transaction.input[0].witness.push([3; 64]);
        observation.transaction.input[0].witness.push([4; 33]);
        builder.verify_observed_withdrawal(&observation, &requests, &config)?;
        let mut wrong = requests.clone();
        wrong[0].amount = Amount::from_sat(255);
        assert!(builder
            .verify_observed_withdrawal(&observation, &wrong, &config)
            .is_err());
        observation.withdrawals[0].vout = 1;
        assert!(builder
            .verify_observed_withdrawal(&observation, &requests, &config)
            .is_err());
        Ok(())
    }

    #[test]
    fn fixed_construction_rejects_amount_and_fee_overflow() -> Result<()> {
        let (builder, config, parent, mut outputs) = fixed_fixture(u64::MAX, u64::MAX);
        let inputs = vec![(
            OutPoint::new(parent.compute_txid(), 0),
            parent.output[0].clone(),
        )];
        assert!(builder
            .build_fixed_bridge_tx(&inputs, outputs.clone(), &config, u64::MAX)
            .is_err());
        outputs.push(outputs[0].clone());
        assert!(builder
            .build_fixed_bridge_tx(&inputs, outputs, &config, 1)
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn fixed_rounding_preserves_equal_split_and_never_drops_a_request() -> Result<()> {
        let (builder, config, parent, mut outputs) = fixed_fixture(4_000, 1_000);
        let inputs = vec![(
            OutPoint::new(parent.compute_txid(), 0),
            parent.output[0].clone(),
        )];
        for reference in [2u8, 3u8] {
            let mut output = outputs[0].clone();
            output.op_return_data = Some(vec![reference; 10]);
            outputs.push(output);
        }
        let fixed = builder.build_fixed_bridge_tx(&inputs, outputs.clone(), &config, 1)?;
        assert_eq!(fixed.fee.to_sat(), 324);
        assert_eq!(fixed.tx.output[0].value.to_sat(), 892);
        assert_eq!(fixed.tx.output[1].value.to_sat(), 892);
        assert_eq!(fixed.tx.output[2].value.to_sat(), 892);
        assert_eq!(fixed.change_amount.to_sat(), 1_000);
        let produced = builder
            .build_bridge_txs(inputs.clone(), outputs.clone(), config.clone(), 1)
            .await?;
        assert_eq!(fixed, produced[0]);
        outputs[1].output.value = Amount::from_sat(107);
        assert!(builder
            .build_fixed_bridge_tx(&inputs, outputs, &config, 1)
            .is_err());
        Ok(())
    }
}
