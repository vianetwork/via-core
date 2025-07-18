#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use anyhow::Result;
    use async_trait::async_trait;
    use bitcoin::{
        hashes::Hash,
        secp256k1::{Secp256k1, SecretKey},
        Address, Amount, CompressedPublicKey, Network, NetworkKind, OutPoint, PrivateKey,
        ScriptBuf, Transaction, TxOut, Txid,
    };
    use bitcoincore_rpc::json::GetBlockStatsResult;
    use mockall::{mock, predicate::*};
    use rand::{rngs::OsRng, RngCore};
    use via_btc_client::{traits::BitcoinOps, types::BitcoinError};
    use via_verifier_types::transaction::UnsignedBridgeTx;

    use crate::{fee::WithdrawalFeeStrategy, transaction_builder::TransactionBuilder};

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

    const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL:";

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

        TransactionBuilder::new(btc_client, get_bridge_address_mock())
    }

    fn create_random_outputs(withdrawal_values: Vec<Amount>) -> Vec<TxOut> {
        let mut outputs = vec![];
        for value in &withdrawal_values {
            outputs.push(TxOut {
                script_pubkey: generate_wallet_address(get_network()).script_pubkey(),
                value: *value,
            });
        }
        outputs
    }

    async fn create_bridge_tx(
        bridge_address_total_values: Vec<Amount>,
        users_requested_value: Vec<Amount>,
        proof_txid: Txid,
    ) -> anyhow::Result<UnsignedBridgeTx> {
        let btc_client = create_btc_client_mock(bridge_address_total_values.clone());
        let builder = create_tx_builder_mock(Some(btc_client))?;

        let outputs = create_random_outputs(users_requested_value.clone());

        let bridge_tx = builder
            .build_transaction_with_op_return(
                outputs.clone(),
                OP_RETURN_WITHDRAW_PREFIX,
                vec![proof_txid.as_raw_hash().to_byte_array()],
                Arc::new(WithdrawalFeeStrategy::new()),
                None,
            )
            .await?;

        let mut i = 0;
        // Verify the order of the users based on the script_bytes
        for output in outputs {
            if output.value < bridge_tx.fee {
                continue;
            }
            assert_eq!(bridge_tx.tx.output[i].script_pubkey, output.script_pubkey);
            i += 1;
        }

        Ok(bridge_tx)
    }

    #[tokio::test]
    async fn test_withdrawal_builder_one_user_and_value_greater_than_tx_fee() -> Result<()> {
        let bridge_address_total_value = vec![Amount::from_btc(1.0)?];
        let user_requested_value = vec![Amount::from_btc(0.1)?];
        let proof_txid = Txid::from_str(&generate_random_hex_string())?;

        let bridge_tx = create_bridge_tx(
            bridge_address_total_value.clone(),
            user_requested_value.clone(),
            proof_txid.clone(),
        )
        .await?;

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

        // Check if the reveal tx is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .ends_with(proof_txid.as_byte_array()));

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_many_users_and_value_greater_than_tx_fee() -> Result<()> {
        let bridge_address_total_values = vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?];
        let users_requested_value = vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?];
        let proof_txid = Txid::from_str(&generate_random_hex_string())?;

        let bridge_tx = create_bridge_tx(
            bridge_address_total_values.clone(),
            users_requested_value.clone(),
            proof_txid.clone(),
        )
        .await?;

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

        for (i, value) in users_requested_value.iter().enumerate() {
            let user1_output = bridge_tx.tx.output[i].clone();

            // The user should pay 1/2 total fee (there are 2 users).
            assert_eq!(user1_output.value + fee_per_user, *value);
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

        // Check if the reveal tx is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .ends_with(proof_txid.as_byte_array()));

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_many_users_but_one_user_value_less_than_tx_fee() -> Result<()>
    {
        let bridge_address_total_values = vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?];
        // This user requested a small value, it should be ignored when process withdrawal
        let users_request_small_value = Amount::from_sat(20);
        let users_requested_value = vec![
            Amount::from_sat(100000000),
            Amount::from_sat(99999980),
            users_request_small_value,
        ];
        let proof_txid = Txid::from_str(&generate_random_hex_string())?;

        let bridge_tx = create_bridge_tx(
            bridge_address_total_values.clone(),
            users_requested_value.clone(),
            proof_txid.clone(),
        )
        .await?;

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

        for (i, value) in users_requested_value.iter().enumerate() {
            if *value < fee_per_user {
                continue;
            }
            let user1_output = bridge_tx.tx.output[i].clone();

            // The user should pay 1/2 total fee (there are 2 users).
            assert_eq!(user1_output.value + fee_per_user, *value);
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

        // Check if the reveal tx is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .ends_with(proof_txid.as_byte_array()));

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_many_users_but_one_user_value_less_than_tx_fee_when_first(
    ) -> Result<()> {
        let bridge_address_total_values = vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?];
        // This user requested a small value, it should be ignored when process withdrawal
        let users_request_small_value = Amount::from_sat(20);
        let users_requested_value = vec![
            users_request_small_value,
            Amount::from_sat(100000000),
            Amount::from_sat(99999980),
        ];
        let proof_txid = Txid::from_str(&generate_random_hex_string())?;

        let bridge_tx = create_bridge_tx(
            bridge_address_total_values.clone(),
            users_requested_value.clone(),
            proof_txid.clone(),
        )
        .await?;

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
        for value in users_requested_value {
            if value < fee_per_user {
                continue;
            }
            let user1_output = bridge_tx.tx.output[i].clone();

            // The user should pay 1/2 total fee (there are 2 users).
            assert_eq!(user1_output.value + fee_per_user, value);
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

        // Check if the reveal tx is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .ends_with(proof_txid.as_byte_array()));

        Ok(())
    }

    #[tokio::test]
    async fn test_withdrawal_builder_many_users_but_all_value_less_than_tx_fee() -> Result<()> {
        let bridge_address_total_values = vec![Amount::from_btc(1.0)?, Amount::from_btc(1.0)?];
        // This user requested a small value, it should be ignored when process withdrawal
        let users_request_small_value = Amount::from_sat(20);
        let users_requested_value = vec![
            users_request_small_value,
            users_request_small_value,
            users_request_small_value,
        ];
        let proof_txid = Txid::from_str(&generate_random_hex_string())?;

        let bridge_tx = create_bridge_tx(
            bridge_address_total_values.clone(),
            users_requested_value.clone(),
            proof_txid.clone(),
        )
        .await?;

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

        // Check if the reveal tx is included
        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .ends_with(proof_txid.as_byte_array()));

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
            TxOut {
                script_pubkey: ScriptBuf::new(),
                value: Amount::from_sat(1000),
            },
            TxOut {
                script_pubkey: ScriptBuf::new(),
                value: Amount::from_sat(1000),
            },
        ];

        let fee_rate = 1;
        let total_requested = outputs.iter().map(|output| output.value).sum::<Amount>();
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let (adjusted_outputs, adjusted_total_value_needed, actual_fee, adjusted_selected_utxos) =
            builder
                .prepare_build_transaction(
                    outputs.clone(),
                    &available_utxos,
                    fee_rate,
                    fee_strategy,
                )
                .await?;
        assert_eq!(adjusted_total_value_needed + actual_fee, total_requested);
        assert_eq!(adjusted_outputs.len(), outputs.len());

        // Check if the fee is applied to the outputs
        for (i, output) in adjusted_outputs.iter().enumerate() {
            assert_eq!(
                output.value + Amount::from_sat(actual_fee.to_sat() / outputs.len() as u64),
                outputs[i].value
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
            TxOut {
                script_pubkey: ScriptBuf::new(),
                value: Amount::from_sat(1500),
            },
            TxOut {
                script_pubkey: ScriptBuf::new(),
                value: Amount::from_sat(500),
            },
        ];

        let fee_rate = 1;
        let total_requested = outputs.iter().map(|output| output.value).sum::<Amount>();
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let (adjusted_outputs, adjusted_total_value_needed, actual_fee, adjusted_selected_utxos) =
            builder
                .prepare_build_transaction(
                    outputs.clone(),
                    &available_utxos,
                    fee_rate,
                    fee_strategy,
                )
                .await?;
        assert_eq!(adjusted_total_value_needed + actual_fee, total_requested);
        assert_eq!(adjusted_outputs.len(), outputs.len());

        // Check if the fee is applied to the outputs
        for (i, output) in adjusted_outputs.iter().enumerate() {
            assert_eq!(
                output.value + Amount::from_sat(actual_fee.to_sat() / outputs.len() as u64),
                outputs[i].value
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
            TxOut {
                script_pubkey: user1_script_pubkey.clone(),
                value: user1_value,
            },
            TxOut {
                script_pubkey: ScriptBuf::new(),
                value: Amount::from_sat(100),
            },
        ];

        let fee_rate = 1;
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let (adjusted_outputs, adjusted_total_value_needed, actual_fee, _) = builder
            .prepare_build_transaction(outputs.clone(), &available_utxos, fee_rate, fee_strategy)
            .await?;
        // The user 2 is not included because his withdrawal value can not cover the fee
        assert_eq!(adjusted_outputs.len(), 1);
        assert_eq!(adjusted_outputs[0].value.clone() + actual_fee, user1_value);
        assert_eq!(adjusted_outputs[0].script_pubkey, user1_script_pubkey);
        assert_eq!(adjusted_total_value_needed + actual_fee, user1_value);

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
            TxOut {
                script_pubkey: ScriptBuf::new(),
                value: Amount::from_sat(100),
            },
            TxOut {
                script_pubkey: ScriptBuf::new(),
                value: Amount::from_sat(100),
            },
        ];

        let fee_rate = 1;
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let (adjusted_outputs, adjusted_total_value_needed, actual_fee, selected_utxos) = builder
            .prepare_build_transaction(outputs.clone(), &available_utxos, fee_rate, fee_strategy)
            .await?;

        assert_eq!(adjusted_outputs.len(), 0);
        assert_ne!(actual_fee, Amount::ZERO);
        assert_eq!(adjusted_total_value_needed, Amount::ZERO);
        assert_eq!(selected_utxos, vec![available_utxos[0].clone()]);

        Ok(())
    }
}
