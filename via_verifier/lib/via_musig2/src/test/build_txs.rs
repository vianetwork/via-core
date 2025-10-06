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
            .build_transaction_with_op_return(outputs.clone(), config)
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
}
