// Withdrawal Builder Service
// This service has main method, that receives a list of bitcoin address and amount to withdraw and also L1batch proofDaReference reveal transaction id
// and then it will use client to get available utxo, and then perform utxo selection based on the total amount of the withdrawal
// and now we know the number of input and output we can estimate the fee and perform final utxo selection
// create a unsigned transaction and return it to the caller
use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use bincode::{deserialize, serialize};
use bitcoin::{
    absolute, hashes::Hash, script::PushBytesBuf, transaction, Address, Amount, OutPoint,
    ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument};
use utxo_manager::{TransactionContext, UtxoManager};

mod utxo_manager;

use crate::{
    client::BitcoinClient,
    traits::{BitcoinOps, Serializable},
    types::BitcoinNetwork,
};

#[derive(Debug)]
pub struct WithdrawalBuilder {
    client: Arc<dyn BitcoinOps>,
    bridge_address: Address,
    utxo_manager: UtxoManager,
}

#[derive(Debug)]
pub struct WithdrawalRequest {
    pub address: Address,
    pub amount: Amount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsignedWithdrawalTx {
    pub tx: Transaction,
    pub txid: Txid,
    pub utxos: Vec<(OutPoint, TxOut)>,
    pub change_amount: Amount,
    pub merge_utxo_tx: Option<Transaction>,
}

impl Serializable for UnsignedWithdrawalTx {
    fn to_bytes(&self) -> Vec<u8> {
        serialize(self).expect("error serialize the UnsignedWithdrawalTx")
    }

    fn from_bytes(bytes: &[u8]) -> Self
    where
        Self: Sized,
    {
        deserialize(bytes).expect("error deserialize the UnsignedWithdrawalTx")
    }
}

const OP_RETURN_PREFIX: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL:";

impl WithdrawalBuilder {
    #[instrument(skip(rpc_url, auth), target = "bitcoin_withdrawal")]
    pub async fn new(
        rpc_url: &str,
        network: BitcoinNetwork,
        auth: bitcoincore_rpc::Auth,
        bridge_address: Address,
        minimum_amount: Amount,
    ) -> Result<Self> {
        info!("Creating new WithdrawalBuilder");
        let client = Arc::new(BitcoinClient::new(rpc_url, network, auth)?);

        Ok(Self {
            client: client.clone(),
            bridge_address: bridge_address.clone(),
            utxo_manager: UtxoManager::new(client, bridge_address, minimum_amount),
        })
    }

    #[instrument(skip(self, withdrawals, proof_txid), target = "bitcoin_withdrawal")]
    pub async fn create_unsigned_withdrawal_tx(
        &mut self,
        withdrawals: Vec<WithdrawalRequest>,
        proof_txid: Txid,
    ) -> Result<UnsignedWithdrawalTx> {
        debug!("Creating unsigned withdrawal transaction");

        println!("000000000");

        // Group withdrawals by address and sum amounts
        let mut grouped_withdrawals: HashMap<Address, Amount> = HashMap::new();
        for w in withdrawals {
            *grouped_withdrawals.entry(w.address).or_insert(Amount::ZERO) = grouped_withdrawals
                .get(&w.address)
                .unwrap_or(&Amount::ZERO)
                .checked_add(w.amount)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
        }
        println!("000000000");

        // Calculate total withdrawal amount from grouped withdrawals
        let total_withdrawal_amount: Amount = grouped_withdrawals
            .values()
            .try_fold(Amount::ZERO, |acc, amount| acc.checked_add(*amount))
            .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow"))?;
        println!("000000000");

        // Get fee rate
        let fee_rate = std::cmp::max(self.client.get_fee_rate(1).await?, 1);
        println!("000000000");

        // Sync UTXO manager with blockchain
        self.utxo_manager.sync_context_with_blockchain().await?;
        println!("000000000");

        // Get merge UTXO transaction if available
        let merge_utxo_metadata = self
            .utxo_manager
            .get_merge_utxo_unsigned_transaction(fee_rate)
            .await?;
        let mut available_utxos: Vec<(OutPoint, TxOut)> = Vec::new();
        let mut merge_utxo_tx: Option<Transaction> = None;
        let mut cpfp_fee: Amount = Amount::ZERO;
        println!("000000000");

        // If merge UTXO transaction is available, create a chained transaction else, get available UTXOs.
        if let Some((merge_unsigned_tx, tx, fee)) = merge_utxo_metadata {
            println!("000222");
            for (i, outpoint) in tx.outpoints.iter().enumerate() {
                available_utxos.push((outpoint.clone(), merge_unsigned_tx.output[i].clone()));
            }
            println!("000222");
            merge_utxo_tx = Some(merge_unsigned_tx);
            println!("000222");
            cpfp_fee = fee;
            self.utxo_manager.insert_transaction(tx);
            println!("000222");
        } else {
            available_utxos = self
                .utxo_manager
                .get_available_utxos()
                .await?
                .iter()
                .map(|(outpoint, txout)| (outpoint.clone(), txout.clone()))
                .collect();
        }
        println!("000111");

        // Estimate initial fee with approximate input count
        // We'll estimate high initially to avoid underestimating
        let estimated_input_count =
            self.estimate_input_count(&available_utxos, total_withdrawal_amount)?;
        let initial_fee = self.estimate_fee(
            estimated_input_count,
            grouped_withdrawals.len() as u32 + 2, // +1 for OP_RETURN, +1 for potential change
            fee_rate,
        )?;

        // Calculate total amount needed including estimated fee
        let total_needed = total_withdrawal_amount
            .checked_add(initial_fee)
            .ok_or_else(|| anyhow::anyhow!("Total amount overflow"))?;

        // Select UTXOs for the total amount including fee
        let selected_utxos = self.select_utxos(&available_utxos, total_needed).await?;

        // Calculate total input amount
        let total_input_amount: Amount = selected_utxos
            .iter()
            .try_fold(Amount::ZERO, |acc, (_, txout)| acc.checked_add(txout.value))
            .ok_or_else(|| anyhow::anyhow!("Input amount overflow"))?;

        // Create OP_RETURN output with proof txid
        let op_return_data = WithdrawalBuilder::create_op_return_script(proof_txid)?;
        let op_return_output = TxOut {
            value: Amount::ZERO,
            script_pubkey: op_return_data,
        };

        // Calculate actual fee with real input count
        let mut actual_fee = self.estimate_fee(
            selected_utxos.len() as u32,
            grouped_withdrawals.len() as u32 + 1, // +1 for OP_RETURN output
            fee_rate,
        )?;

        // Add CPFP fee
        actual_fee += cpfp_fee;

        // Verify we have enough funds with actual fee
        let total_needed = total_withdrawal_amount
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

        // Create outputs for grouped withdrawals
        let mut outputs: Vec<TxOut> = grouped_withdrawals
            .into_iter()
            .map(|(address, amount)| TxOut {
                value: amount,
                script_pubkey: address.script_pubkey(),
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

        self.utxo_manager.insert_transaction(TransactionContext {
            tx: unsigned_tx.clone(),
            outpoints: vec![OutPoint {
                txid,
                vout: outputs.len() as u32 - 1,
            }],
        });

        debug!("Unsigned withdrawal transaction created successfully");

        Ok(UnsignedWithdrawalTx {
            tx: unsigned_tx,
            txid,
            utxos: selected_utxos,
            change_amount,
            merge_utxo_tx,
        })
    }

    #[instrument(skip(self), target = "bitcoin_withdrawal")]
    async fn get_available_utxos(&self) -> Result<Vec<(OutPoint, TxOut)>> {
        let utxos = self.client.fetch_utxos(&self.bridge_address).await?;
        Ok(utxos)
    }

    #[instrument(skip(self, utxos), target = "bitcoin_withdrawal")]
    async fn select_utxos(
        &self,
        utxos: &[(OutPoint, TxOut)],
        target_amount: Amount,
    ) -> Result<Vec<(OutPoint, TxOut)>> {
        // Simple implementation - could be improved with better UTXO selection algorithm
        let mut selected = Vec::new();
        let mut total = Amount::ZERO;

        for utxo in utxos {
            selected.push(utxo.clone());
            total = total
                .checked_add(utxo.1.value)
                .ok_or_else(|| anyhow::anyhow!("Amount overflow during UTXO selection"))?;

            if total >= target_amount {
                break;
            }
        }

        if total < target_amount {
            return Err(anyhow::anyhow!(
                "Insufficient funds: have {}, need {}",
                total,
                target_amount
            ));
        }

        Ok(selected)
    }

    #[instrument(skip(self), target = "bitcoin_withdrawal")]
    fn estimate_fee(&self, input_count: u32, output_count: u32, fee_rate: u64) -> Result<Amount> {
        // Estimate transaction size
        let base_size = 10_u64; // version + locktime
        let input_size = 148_u64 * u64::from(input_count); // approximate size per input
        let output_size = 34_u64 * u64::from(output_count); // approximate size per output

        let total_size = base_size + input_size + output_size;
        let fee = fee_rate * total_size;

        Ok(Amount::from_sat(fee))
    }

    // Helper function to create OP_RETURN script
    pub fn create_op_return_script(proof_txid: Txid) -> Result<ScriptBuf> {
        let mut data = Vec::with_capacity(OP_RETURN_PREFIX.len() + 32);
        data.extend_from_slice(OP_RETURN_PREFIX);
        data.extend_from_slice(&proof_txid.as_raw_hash().to_byte_array());

        let mut encoded_data = PushBytesBuf::with_capacity(data.len());
        encoded_data.extend_from_slice(&data).ok();

        Ok(ScriptBuf::new_op_return(encoded_data))
    }

    #[instrument(skip(self, utxos), target = "bitcoin_withdrawal")]
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
    use std::str::FromStr;

    use async_trait::async_trait;
    use bitcoin::{transaction::Version, Network};
    use mockall::{mock, predicate::*};

    use super::*;
    use crate::{
        inscriber::test_utils::{MockBitcoinOps, MockBitcoinOpsConfig},
        types::BitcoinError,
    };

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

            async fn get_block_stats(&self, _height: u64) -> Result<bitcoincore_rpc::json::GetBlockStatsResult, BitcoinError> {
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
        println!("1111111111111111");
        mock_ops.expect_get_fee_rate().returning(|_| Ok(2));

        let client = Arc::new(MockBitcoinOps::new(MockBitcoinOpsConfig::default()));
        let mut utxo_manager =
            UtxoManager::new(client, bridge_address.clone(), Amount::from_sat(1));

        let tx = Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(200000000),
                script_pubkey: bridge_address.clone().script_pubkey(),
            }],
        };
        let tx_context = TransactionContext {
            tx: tx.clone(),
            outpoints: vec![OutPoint {
                txid: tx.compute_txid(),
                vout: 0,
            }],
        };

        utxo_manager.insert_transaction(tx_context);

        // Use mock client
        let mut builder = WithdrawalBuilder {
            client: Arc::new(MockBitcoinOpsService::new()),
            bridge_address: bridge_address.clone(),
            utxo_manager,
        };
        println!("1111111111111111");

        let withdrawal_address = "bcrt1pv6dtdf0vrrj6ntas926v8vw9u0j3mga29vmfnxh39zfxya83p89qz9ze3l";
        let withdrawal_amount = Amount::from_btc(0.1)?;
        println!("1111111111111111");

        let withdrawals = vec![WithdrawalRequest {
            address: Address::from_str(withdrawal_address)?.require_network(network)?,
            amount: withdrawal_amount,
        }];
        println!("1111111111111111");

        let proof_txid =
            Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")?;
        println!("1111111111111111");

        let withdrawal_tx = builder
            .create_unsigned_withdrawal_tx(withdrawals, proof_txid)
            .await?;
        assert!(!withdrawal_tx.utxos.is_empty());
        println!("1111111111111111");

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
            .windows(OP_RETURN_PREFIX.len())
            .any(|window| window == OP_RETURN_PREFIX));

        Ok(())
    }
}
