use std::sync::Arc;

use anyhow::Result;
use bitcoin::{
    absolute, script::PushBytesBuf, transaction, Address, Amount, OutPoint, ScriptBuf, Sequence,
    Transaction, TxIn, TxOut, Witness,
};
use tracing::{debug, instrument};
use via_btc_client::traits::BitcoinOps;
use via_verifier_types::transaction::UnsignedBridgeTx;

use crate::utxo_manager::UtxoManager;

#[derive(Debug, Clone)]
pub struct TransactionBuilder {
    pub utxo_manager: UtxoManager,
    bridge_address: Address,
}

impl TransactionBuilder {
    #[instrument(skip(btc_client), target = "bitcoin_transaction_builder")]
    pub fn new(btc_client: Arc<dyn BitcoinOps>, bridge_address: Address) -> Result<Self> {
        let utxo_manager = UtxoManager::new(
            btc_client.clone(),
            bridge_address.clone(),
            Amount::from_sat(1000),
            128,
        );

        Ok(Self {
            utxo_manager,
            bridge_address,
        })
    }

    #[instrument(
        skip(self, outputs, op_return_data),
        target = "bitcoin_transaction_builder"
    )]
    pub async fn build_transaction_with_op_return(
        &self,
        mut outputs: Vec<TxOut>,
        op_return_prefix: &[u8],
        op_return_data: Vec<[u8; 32]>,
    ) -> Result<UnsignedBridgeTx> {
        self.utxo_manager.sync_context_with_blockchain().await?;

        // Calculate total rquired amount.
        let mut total_required_amount: Amount = Amount::ZERO;
        for output in &outputs {
            total_required_amount = total_required_amount
                .checked_add(output.value)
                .ok_or_else(|| anyhow::anyhow!("Amount overflow"))?;
        }

        // Get available UTXOs first to estimate number of inputs
        let available_utxos = self.utxo_manager.get_available_utxos().await?;

        // Get fee rate
        let fee_rate = std::cmp::max(self.utxo_manager.get_btc_client().get_fee_rate(1).await?, 1);

        // Estimate initial fee with approximate input count
        // We'll estimate high initially to avoid underestimating
        let estimated_input_count =
            self.estimate_input_count(&available_utxos, total_required_amount)?;
        let initial_fee = self.estimate_fee(
            estimated_input_count,
            outputs.len() as u32 + 2, // +1 for OP_RETURN, +1 for potential change
            fee_rate,
        )?;

        // Calculate total amount needed including estimated fee
        let total_needed = total_required_amount
            .checked_add(initial_fee)
            .ok_or_else(|| anyhow::anyhow!("Total amount overflow"))?;

        // Select UTXOs for the total amount including fee
        let selected_utxos = self
            .utxo_manager
            .select_utxos_by_target_value(&available_utxos, total_needed)
            .await?;

        // Calculate total input amount
        let total_input_amount: Amount = selected_utxos
            .iter()
            .try_fold(Amount::ZERO, |acc, (_, txout)| acc.checked_add(txout.value))
            .ok_or_else(|| anyhow::anyhow!("Input amount overflow"))?;

        // Create OP_RETURN output with proof txid
        let op_return_data =
            TransactionBuilder::create_op_return_script(op_return_prefix, op_return_data)?;

        let op_return_output = TxOut {
            value: Amount::ZERO,
            script_pubkey: op_return_data,
        };

        // Calculate actual fee with real input count
        let actual_fee = self.estimate_fee(
            selected_utxos.len() as u32,
            outputs.len() as u32 + 1, // +1 for OP_RETURN output
            fee_rate,
        )?;

        // Verify we have enough funds with actual fee
        let total_needed = total_required_amount
            .checked_add(actual_fee)
            .ok_or_else(|| anyhow::anyhow!("Total amount overflow"))?;

        if total_input_amount < total_needed {
            return Err(anyhow::anyhow!(
                "Insufficient funds: have {}, need {}",
                total_input_amount,
                total_needed
            ));
        }

        // Create inputs
        let inputs: Vec<TxIn> = selected_utxos
            .iter()
            .map(|(outpoint, _)| TxIn {
                previous_output: *outpoint,
                script_sig: ScriptBuf::default(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(),
            })
            .collect();

        // Add OP_RETURN output
        outputs.push(op_return_output);

        // Add change output if needed
        let change_amount = total_input_amount
            .checked_sub(total_needed)
            .ok_or_else(|| anyhow::anyhow!("Change amount calculation overflow"))?;

        if change_amount.to_sat() > 0 {
            outputs.push(TxOut {
                value: change_amount,
                script_pubkey: self.bridge_address.script_pubkey(),
            });
        }

        // Create unsigned transaction
        let unsigned_tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs,
            output: outputs.clone(),
        };

        let txid = unsigned_tx.compute_txid();

        self.utxo_manager
            .insert_transaction(unsigned_tx.clone())
            .await;

        debug!("Unsigned created successfully");

        Ok(UnsignedBridgeTx {
            tx: unsigned_tx,
            txid,
            utxos: selected_utxos,
            change_amount,
        })
    }

    // Helper function to create OP_RETURN script
    pub fn create_op_return_script(prefix: &[u8], inputs: Vec<[u8; 32]>) -> Result<ScriptBuf> {
        let mut data = Vec::with_capacity(prefix.len() + 32);
        data.extend_from_slice(prefix);
        for input in inputs {
            data.extend_from_slice(&input);
        }

        let mut encoded_data = PushBytesBuf::with_capacity(data.len());
        encoded_data.extend_from_slice(&data).ok();

        Ok(ScriptBuf::new_op_return(encoded_data))
    }

    #[instrument(skip(self), target = "bitcoin_transaction_builder")]
    fn estimate_fee(&self, input_count: u32, output_count: u32, fee_rate: u64) -> Result<Amount> {
        // Estimate transaction size
        let base_size = 10_u64; // version + locktime
        let input_size = 148_u64 * u64::from(input_count); // approximate size per input
        let output_size = 34_u64 * u64::from(output_count); // approximate size per output

        let total_size = base_size + input_size + output_size;
        let fee = fee_rate * total_size;

        Ok(Amount::from_sat(fee))
    }

    #[instrument(skip(self, utxos), target = "bitcoin_transaction_builder")]
    fn estimate_input_count(
        &self,
        utxos: &[(OutPoint, TxOut)],
        target_amount: Amount,
    ) -> Result<u32> {
        let mut count: u32 = 0;
        let mut total = Amount::ZERO;

        for utxo in utxos {
            count += 1;
            total = total
                .checked_add(utxo.1.value)
                .ok_or_else(|| anyhow::anyhow!("Amount overflow during input count estimation"))?;

            if total >= target_amount {
                break;
            }
        }
        // Add one more to our estimate to be safe
        Ok(count.saturating_add(1))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, str::FromStr};

    use async_trait::async_trait;
    use bitcoin::{hashes::Hash, Network, Txid};
    use bitcoincore_rpc::json::GetBlockStatsResult;
    use mockall::{mock, predicate::*};
    use via_btc_client::types::BitcoinError;
    use via_verifier_types::withdrawal::WithdrawalRequest;

    use super::*;

    mock! {
        BitcoinOpsService {}
        #[async_trait]
        impl BitcoinOps for BitcoinOpsService {
            async fn fetch_utxos(&self, _address: &Address) -> Result<Vec<(OutPoint, TxOut)>, BitcoinError> {
                // Mock implementation
                let txid = Txid::from_str(
                    "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b"
                ).unwrap();
                let outpoint = OutPoint::new(txid, 0);
                let txout = TxOut {
                    value: Amount::from_btc(1.0).unwrap(),
                    script_pubkey: ScriptBuf::new(),
                };
                Ok(vec![(outpoint, txout)])
            }

            async fn get_fee_rate(&self, _target_blocks: u16) -> Result<u64, BitcoinError> {
                Ok(2)
            }

            async fn broadcast_signed_transaction(&self, _tx_hex: &str) -> Result<Txid, BitcoinError> {
                Ok(Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b").unwrap())
            }

            async fn check_tx_confirmation(&self, _txid: &Txid, _min_confirmations: u32) -> Result<bool, BitcoinError> {
                Ok(true)
            }

            async fn fetch_block_height(&self) -> Result<u128, BitcoinError> {
                Ok(100000)
            }

            async fn get_balance(&self, _address: &Address) -> Result<u128, BitcoinError> {
                Ok(100000000) // 1 BTC in sats
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
                todo!()
            }

            async fn get_fee_history(&self, _start: usize, _end: usize) -> Result<Vec<u64>, BitcoinError> {
                Ok(vec![1])
            }
        }
    }

    #[tokio::test]
    async fn test_withdrawal_builder() -> Result<()> {
        let network = Network::Regtest;
        let bridge_address =
            Address::from_str("bcrt1pxqkh0g270lucjafgngmwv7vtgc8mk9j5y4j8fnrxm77yunuh398qfv8tqp")?
                .require_network(network)?;

        // Create mock and set expectations
        let mut mock_ops = MockBitcoinOpsService::new();
        mock_ops.expect_fetch_utxos().returning(|_| {
            let txid =
                Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
                    .unwrap();
            let outpoint = OutPoint::new(txid, 0);
            let txout = TxOut {
                value: Amount::from_btc(1.0).unwrap(),
                script_pubkey: ScriptBuf::new(),
            };
            Ok(vec![(outpoint, txout)])
        });

        mock_ops.expect_get_fee_rate().returning(|_| Ok(2));

        let btc_client = Arc::new(mock_ops);
        let utxo_manager = UtxoManager::new(
            btc_client.clone(),
            bridge_address.clone(),
            Amount::ZERO,
            128,
        );
        // Use mock client
        let builder = TransactionBuilder {
            utxo_manager,
            bridge_address,
        };

        let withdrawal_address = "bcrt1pv6dtdf0vrrj6ntas926v8vw9u0j3mga29vmfnxh39zfxya83p89qz9ze3l";
        let withdrawal_amount = Amount::from_btc(0.1)?;

        let withdrawals = vec![WithdrawalRequest {
            address: Address::from_str(withdrawal_address)?.require_network(network)?,
            amount: withdrawal_amount,
        }];

        let mut grouped_withdrawals: HashMap<Address, Amount> = HashMap::new();
        for w in withdrawals {
            *grouped_withdrawals.entry(w.address).or_insert(Amount::ZERO) = grouped_withdrawals
                .get(&w.address)
                .unwrap_or(&Amount::ZERO)
                .checked_add(w.amount)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
        }

        // Create outputs for grouped withdrawals
        let outputs: Vec<TxOut> = grouped_withdrawals
            .into_iter()
            .map(|(address, amount)| TxOut {
                value: amount,
                script_pubkey: address.script_pubkey(),
            })
            .collect();

        let proof_txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")?;

        const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL:";

        let withdrawal_tx = builder
            .build_transaction_with_op_return(
                outputs,
                OP_RETURN_WITHDRAW_PREFIX,
                vec![proof_txid.as_raw_hash().to_byte_array()],
            )
            .await?;
        assert!(!withdrawal_tx.utxos.is_empty());

        // Verify OP_RETURN output
        let op_return_output = withdrawal_tx
            .tx
            .output
            .iter()
            .find(|output| output.script_pubkey.is_op_return())
            .expect("OP_RETURN output not found");

        assert!(op_return_output
            .script_pubkey
            .as_bytes()
            .windows(OP_RETURN_WITHDRAW_PREFIX.len())
            .any(|window| window == OP_RETURN_WITHDRAW_PREFIX));

        Ok(())
    }
}
