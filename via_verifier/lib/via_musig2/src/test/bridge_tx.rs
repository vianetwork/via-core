#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use anyhow::Result;
    use async_trait::async_trait;
    use bitcoin::{
        policy::MAX_STANDARD_TX_WEIGHT, Address, Amount, Network, OutPoint, ScriptBuf, Transaction,
        TxOut, Txid,
    };
    use bitcoincore_rpc::json::GetBlockStatsResult;
    use mockall::{mock, predicate::*};
    use rand::RngCore;
    use via_btc_client::{traits::BitcoinOps, types::BitcoinError};

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

    fn dummy_outpoint(index: u32) -> OutPoint {
        OutPoint {
            txid: Txid::from_str(&generate_random_hex_string()).unwrap(),
            vout: index,
        }
    }

    fn dummy_txout(value: u64) -> TxOut {
        TxOut {
            value: bitcoin::Amount::from_sat(value),
            script_pubkey: ScriptBuf::new(),
        }
    }

    pub fn generate_dummy_utxos(count: usize, value: u64) -> Vec<(OutPoint, TxOut)> {
        (0..count)
            .map(|i| (dummy_outpoint(i as u32), dummy_txout(value)))
            .collect()
    }

    pub fn generate_dummy_outputs(count: usize, value: u64) -> Vec<TxOut> {
        (0..count).map(|_| dummy_txout(value)).collect()
    }

    #[tokio::test]
    async fn test_get_transaction_metadata_when_one_tx() -> Result<()> {
        let tx_builder = create_tx_builder_mock(None)?;
        let available_utxos = generate_dummy_utxos(1, 10000000);
        let outputs = generate_dummy_outputs(5, 2000000);
        let fee_rate = 1;
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let txs_metadata = tx_builder
            .get_transaction_metadata(&available_utxos, &outputs, fee_rate, fee_strategy)
            .await?;

        assert_eq!(txs_metadata.len(), 1);
        assert_eq!(txs_metadata[0].inputs.len(), available_utxos.len());
        assert_eq!(txs_metadata[0].outputs.len(), outputs.len());
        assert_eq!(
            txs_metadata[0].total_amount + txs_metadata[0].fee,
            available_utxos[0].1.value
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_transaction_metadata_when_many_tx_case_all_inputs_included() -> Result<()> {
        let tx_builder = create_tx_builder_mock(None)?;
        let available_utxos = generate_dummy_utxos(4000, 2000000);
        let outputs = generate_dummy_outputs(4000, 2000000);
        let fee_rate = 1;
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let total_weight = tx_builder
            .estimate_transaction_weight(available_utxos.len() as u64, outputs.len() as u64);
        let expected_bridge_tx =
            (total_weight as f64 / MAX_STANDARD_TX_WEIGHT as f64).ceil() as usize;

        let txs_metadata = tx_builder
            .get_transaction_metadata(&available_utxos, &outputs, fee_rate, fee_strategy)
            .await?;

        assert_eq!(txs_metadata.len(), expected_bridge_tx);

        let mut total_amount_input: u64 = 0;
        let mut total_input: usize = 0;
        let mut total_amount_output: u64 = 0;
        let mut total_output: usize = 0;
        let mut total_fee: u64 = 0;

        for tx_metadata in txs_metadata {
            let weight = tx_builder.estimate_transaction_weight(
                tx_metadata.inputs.len() as u64,
                tx_metadata.outputs.len() as u64,
            );
            assert!(weight < MAX_STANDARD_TX_WEIGHT as u64);

            let tx_total_input = tx_metadata
                .inputs
                .iter()
                .map(|input| input.1.value.to_sat())
                .sum::<u64>();

            total_amount_input += tx_total_input;
            total_input += tx_metadata.inputs.len();

            let tx_total_output = tx_metadata
                .outputs
                .iter()
                .map(|output| output.value.to_sat())
                .sum::<u64>();
            total_amount_output += tx_total_output;
            total_output += tx_metadata.outputs.len();
            total_fee += tx_metadata.fee.to_sat();

            assert!(tx_total_output > 0);
            assert!(tx_total_input > 0);
            assert_eq!(tx_total_output + tx_metadata.fee.to_sat(), tx_total_input);
        }

        assert_eq!(total_input, available_utxos.len());
        assert_eq!(total_output, outputs.len());
        assert_eq!(total_amount_input, total_fee + total_amount_output);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_transaction_metadata_when_many_tx_case_some_inputs_included() -> Result<()> {
        let tx_builder = create_tx_builder_mock(None)?;
        let available_utxos = generate_dummy_utxos(4000, 2000000);
        let outputs = generate_dummy_outputs(2000, 2000000);
        let fee_rate = 1;
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let total_weight = tx_builder
            .estimate_transaction_weight(available_utxos.len() as u64 / 2, outputs.len() as u64);
        let expected_bridge_tx =
            (total_weight as f64 / MAX_STANDARD_TX_WEIGHT as f64).ceil() as usize;

        let txs_metadata = tx_builder
            .get_transaction_metadata(&available_utxos, &outputs, fee_rate, fee_strategy)
            .await?;

        assert_eq!(txs_metadata.len(), expected_bridge_tx);

        let mut total_amount_input: u64 = 0;
        let mut total_input: usize = 0;
        let mut total_amount_output: u64 = 0;
        let mut total_output: usize = 0;
        let mut total_fee: u64 = 0;

        for tx_metadata in txs_metadata {
            let weight = tx_builder.estimate_transaction_weight(
                tx_metadata.inputs.len() as u64,
                tx_metadata.outputs.len() as u64,
            );
            assert!(weight < MAX_STANDARD_TX_WEIGHT as u64);

            let tx_total_input = tx_metadata
                .inputs
                .iter()
                .map(|input| input.1.value.to_sat())
                .sum::<u64>();

            total_amount_input += tx_total_input;
            total_input += tx_metadata.inputs.len();

            let tx_total_output = tx_metadata
                .outputs
                .iter()
                .map(|output| output.value.to_sat())
                .sum::<u64>();
            total_amount_output += tx_total_output;
            total_output += tx_metadata.outputs.len();
            total_fee += tx_metadata.fee.to_sat();

            assert!(tx_total_output > 0);
            assert!(tx_total_input > 0);
            assert_eq!(tx_total_output + tx_metadata.fee.to_sat(), tx_total_input);
        }

        assert_eq!(total_input, available_utxos.len() / 2);
        assert_eq!(total_output, outputs.len());
        assert_eq!(total_amount_input, total_fee + total_amount_output);

        Ok(())
    }

    #[tokio::test]
    async fn test_build_bridge_txs() -> Result<()> {
        let tx_builder = create_tx_builder_mock(None)?;
        let available_utxos = generate_dummy_utxos(4000, 2000000);
        let outputs = generate_dummy_outputs(4000, 2000000);
        let fee_rate = 1;
        let fee_strategy = Arc::new(WithdrawalFeeStrategy::new());

        let txs_metadata = tx_builder
            .get_transaction_metadata(&available_utxos, &outputs, fee_rate, fee_strategy)
            .await?;

        let op_return_output_base = TxOut {
            value: Amount::from_sat(0),
            script_pubkey: ScriptBuf::new(),
        };
        let bridge_txs =
            tx_builder.build_bridge_txs(&txs_metadata, fee_rate, op_return_output_base.clone())?;
        assert_eq!(txs_metadata.len(), bridge_txs.len());

        for (i, bridge_tx) in bridge_txs.iter().enumerate() {
            // Check if the index was included in the OP_RETURN data
            let op_return_output = bridge_tx.tx.output[bridge_tx.tx.output.len() - 1].clone();
            if i == 0 {
                assert_eq!(
                    op_return_output_base.script_pubkey,
                    op_return_output.script_pubkey
                );
            } else {
                assert_ne!(
                    op_return_output_base.script_pubkey.to_hex_string(),
                    op_return_output.script_pubkey.to_hex_string()
                );

                let op_return_bytes = op_return_output.script_pubkey.as_bytes().to_vec().clone();
                assert_eq!(*op_return_bytes.last().unwrap(), i as u8);
            }
        }

        Ok(())
    }
}
