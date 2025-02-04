use std::{collections::VecDeque, sync::Arc};

use bitcoin::{
    absolute, transaction, Address, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn,
    TxOut, Witness,
};

use crate::traits::BitcoinOps;

const CTX_REQUIRED_CONFIRMATIONS: u32 = 1;
const MERGE_LIMIT: usize = 128;
const DEFAULT_CAPACITY: usize = 100;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct TransactionContext {
    pub tx: Transaction,
    pub outpoints: Vec<OutPoint>,
}

#[derive(Debug)]
pub struct UtxoManager {
    client: Arc<dyn BitcoinOps>,
    address: Address,
    context: VecDeque<TransactionContext>,
    /// The minimum amount to merge utxos
    minimum_amount: Amount,
}

impl UtxoManager {
    pub fn new(client: Arc<dyn BitcoinOps>, address: Address, minimum_amount: Amount) -> Self {
        UtxoManager {
            client,
            address,
            context: VecDeque::with_capacity(DEFAULT_CAPACITY),
            minimum_amount,
        }
    }

    pub async fn get_available_utxos(&self) -> anyhow::Result<Vec<(OutPoint, TxOut)>> {
        // fetch utxos from client
        let mut utxos = self.client.fetch_utxos(&self.address).await?;
        if self.context.is_empty() {
            return Ok(utxos);
        }

        // Add the output utxos to the list
        for tx in self.context.iter() {
            for (i, out) in tx.tx.output.iter().enumerate() {
                if out.script_pubkey != self.address.script_pubkey() {}
                utxos.push((tx.outpoints[i].clone(), tx.tx.output[i].clone()));
            }
        }

        // Remove the inputs used utxos
        for tx in self.context.iter() {
            for input in tx.tx.input.iter() {
                let outpoint = input.previous_output;
                let index = utxos.iter().position(|(op, _)| op == &outpoint);
                if let Some(index) = index {
                    utxos.remove(index);
                }
            }
        }

        Ok(utxos)
    }

    pub async fn get_utxos_to_merge(&self) -> anyhow::Result<Vec<(OutPoint, TxOut)>> {
        let mut utxos_to_merge = Vec::new();
        let available_utxos = self.get_available_utxos().await?;

        // If the amount is greater than the minimum amount to merge
        for (outpoint, txout) in available_utxos.iter() {
            if txout.value >= self.minimum_amount {
                utxos_to_merge.push((outpoint.clone(), txout.clone()));
            }
        }
        Ok(utxos_to_merge)
    }

    pub async fn sync_context_with_blockchain(&mut self) -> anyhow::Result<()> {
        if self.context.is_empty() {
            return Ok(());
        }

        while let Some(tx) = self.context.pop_front() {
            let res = self
                .client
                .check_tx_confirmation(&tx.tx.compute_txid(), CTX_REQUIRED_CONFIRMATIONS)
                .await?;

            if !res {
                self.context.push_front(tx);
                break;
            }
        }

        Ok(())
    }

    pub async fn get_merge_utxo_unsigned_transaction(
        &self,
        fee_rate: u64,
    ) -> anyhow::Result<Option<(Transaction, TransactionContext, Amount)>> {
        let utxos_to_merge = self.get_utxos_to_merge().await?;

        if utxos_to_merge.is_empty() {
            return Ok(None);
        }

        let mut total_amount = Amount::ZERO;
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();

        for (outpoint, txout) in utxos_to_merge.iter() {
            // Limit the total number of inputs per transaction
            if inputs.len() >= MERGE_LIMIT {
                break;
            }
            total_amount += txout.value;

            inputs.push(TxIn {
                previous_output: outpoint.clone(),
                script_sig: ScriptBuf::default(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(),
            });
        }

        let fees = self.estimate_fee(inputs.len() as u32, 1, fee_rate)?;

        if total_amount < fees {
            return Err(anyhow::anyhow!(
                "Insufficient funds to pay transaction fee: have {}, need {}",
                total_amount,
                fees
            ));
        }

        // Calculate the child pays for parent fee.
        // We will use half of the fee for the CPFP fee and the child will pay the other half.
        let cpfp_fee = fees / 2;

        outputs.push(TxOut {
            value: total_amount - (fees - cpfp_fee),
            script_pubkey: self.address.script_pubkey(),
        });

        let unsigned_tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs,
            output: outputs,
        };

        let txid = unsigned_tx.compute_txid();
        let tx = TransactionContext {
            tx: unsigned_tx.clone(),
            outpoints: vec![OutPoint { txid, vout: 0 }],
        };

        Ok(Some((unsigned_tx, tx, cpfp_fee)))
    }

    pub fn insert_transaction(&mut self, tx: TransactionContext) {
        self.context.push_back(tx);
    }

    fn estimate_fee(
        &self,
        input_count: u32,
        output_count: u32,
        fee_rate: u64,
    ) -> anyhow::Result<Amount> {
        // Estimate transaction size
        let base_size = 10_u64; // version + locktime
        let input_size = 148_u64 * u64::from(input_count); // approximate size per input
        let output_size = 34_u64 * u64::from(output_count); // approximate size per output

        let total_size = base_size + input_size + output_size;
        let fee = fee_rate * total_size;

        Ok(Amount::from_sat(fee))
    }
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use bitcoin::{transaction::Version, Network, Script, Txid};

    use super::*;
    use crate::inscriber::test_utils::{MockBitcoinOps, MockBitcoinOpsConfig};

    fn create_mock_utxo_manager(minimum_amount: Amount) -> UtxoManager {
        let client = Arc::new(MockBitcoinOps::new(MockBitcoinOpsConfig::default()));
        let address = Address::p2pkh(
            &bitcoin::PublicKey::from_slice(&[0x02; 33]).unwrap(),
            Network::Bitcoin,
        );
        UtxoManager::new(client, address, minimum_amount)
    }

    #[tokio::test]
    async fn test_get_available_utxos() {
        let manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let utxos = manager.get_available_utxos().await.unwrap();
        assert!(utxos.is_empty());
    }

    #[tokio::test]
    async fn test_get_utxos_to_merge() {
        let manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let utxos_to_merge = manager.get_utxos_to_merge().await.unwrap();
        assert!(utxos_to_merge.is_empty());
    }

    #[tokio::test]
    async fn test_sync_context_with_blockchain() {
        let mut manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let result = manager.sync_context_with_blockchain().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_merge_utxo_unsigned_transaction() {
        let manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let fee_rate = 1;
        let result = manager
            .get_merge_utxo_unsigned_transaction(fee_rate)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_insert_transaction() {
        let mut manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let tx = Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![],
        };
        let tx_context = TransactionContext {
            tx,
            outpoints: vec![OutPoint {
                txid: Txid::from_str(
                    "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
                )
                .unwrap(),
                vout: 0,
            }],
        };
        manager.insert_transaction(tx_context.clone());
        assert_eq!(manager.context.len(), 1);
        assert_eq!(manager.context.front().unwrap(), &tx_context);
    }

    #[test]
    fn test_estimate_fee() {
        let manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let fee = manager.estimate_fee(1, 1, 1).unwrap();
        assert_eq!(fee, Amount::from_sat(192));
    }

    #[tokio::test]
    async fn test_get_available_utxos_with_context() {
        let mut manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let tx = Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: manager.address.script_pubkey(),
            }],
        };
        let tx_context = TransactionContext {
            tx,
            outpoints: vec![OutPoint {
                txid: Txid::from_str(
                    "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
                )
                .unwrap(),
                vout: 0,
            }],
        };
        manager.insert_transaction(tx_context);
        let utxos = manager.get_available_utxos().await.unwrap();
        assert_eq!(utxos.len(), 1);
    }

    #[tokio::test]
    async fn test_get_merge_utxo_unsigned_transaction_with_utxos() {
        let mut manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let tx = Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(2000),
                script_pubkey: manager.address.script_pubkey(),
            }],
        };
        let tx_context = TransactionContext {
            tx: tx.clone(),
            outpoints: vec![OutPoint {
                txid: tx.compute_txid(),
                vout: 0,
            }],
        };
        manager.insert_transaction(tx_context);
        let fee_rate = 1;
        let result = manager
            .get_merge_utxo_unsigned_transaction(fee_rate)
            .await
            .unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_sync_context_with_blockchain_with_pending_tx() {
        let mut manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let tx = Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(2000),
                script_pubkey: manager.address.script_pubkey(),
            }],
        };
        let tx_context = TransactionContext {
            tx,
            outpoints: vec![OutPoint {
                txid: Txid::from_str(
                    "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
                )
                .unwrap(),
                vout: 0,
            }],
        };
        manager.insert_transaction(tx_context);
        let result = manager.sync_context_with_blockchain().await;
        assert!(result.is_ok());
        assert_eq!(manager.context.len(), 1);
    }

    #[tokio::test]
    async fn test_get_available_utxos_with_mixed_context() {
        let mut manager = create_mock_utxo_manager(Amount::from_sat(1000));
        let tx1 = Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: manager.address.script_pubkey(),
            }],
        };
        let tx2 = Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(2000),
                script_pubkey: manager.address.script_pubkey(),
            }],
        };
        let tx_context1 = TransactionContext {
            tx: tx1.clone(),
            outpoints: vec![OutPoint {
                txid: tx1.compute_txid(),
                vout: 0,
            }],
        };
        let tx_context2 = TransactionContext {
            tx: tx2.clone(),
            outpoints: vec![OutPoint {
                txid: tx2.compute_txid(),
                vout: 0,
            }],
        };
        manager.insert_transaction(tx_context1);
        manager.insert_transaction(tx_context2);
        let utxos = manager.get_available_utxos().await.unwrap();
        assert_eq!(utxos.len(), 2);
    }

    #[tokio::test]
    async fn test_get_merge_utxo_unsigned_transaction_with_insufficient_funds() {
        let mut manager = create_mock_utxo_manager(Amount::from_sat(1));
        let tx = Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(10),
                script_pubkey: manager.address.script_pubkey(),
            }],
        };
        let tx_context = TransactionContext {
            tx: tx.clone(),
            outpoints: vec![OutPoint {
                txid: tx.compute_txid(),
                vout: 0,
            }],
        };
        manager.insert_transaction(tx_context);
        let fee_rate = 1;
        let result = manager.get_merge_utxo_unsigned_transaction(fee_rate).await;
        assert!(result.is_err());
    }
}
