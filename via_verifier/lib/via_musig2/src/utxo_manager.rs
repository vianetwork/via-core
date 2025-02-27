use std::{collections::VecDeque, sync::Arc};

use bitcoin::{Address, Amount, OutPoint, Transaction, TxOut};
use tokio::sync::RwLock;
use via_btc_client::traits::BitcoinOps;

const CTX_REQUIRED_CONFIRMATIONS: u32 = 1;
const DEFAULT_CAPACITY: usize = 100;

#[derive(Debug, Clone)]
pub struct UtxoManager {
    /// Btc client
    btc_client: Arc<dyn BitcoinOps>,
    /// The wallet address
    address: Address,
    /// The transactions executed by the wallet
    context: Arc<RwLock<VecDeque<Transaction>>>,
    /// The minimum amount to merge utxos
    minimum_amount: Amount,
    /// The maximum number of utxos to merge in a single tx
    merge_limit: usize,
}

impl UtxoManager {
    pub fn new(
        btc_client: Arc<dyn BitcoinOps>,
        address: Address,
        minimum_amount: Amount,
        merge_limit: usize,
    ) -> Self {
        UtxoManager {
            btc_client,
            address,
            context: Arc::new(RwLock::new(VecDeque::with_capacity(DEFAULT_CAPACITY))),
            minimum_amount,
            merge_limit,
        }
    }

    pub async fn get_available_utxos(&self) -> anyhow::Result<Vec<(OutPoint, TxOut)>> {
        // fetch utxos from client
        let mut utxos = self.btc_client.fetch_utxos(&self.address).await?;
        let context = self.context.read().await;

        {
            if context.is_empty() {
                return Ok(utxos);
            }
        }

        for tx in context.iter() {
            // Add the output utxos to the list
            for (i, out) in tx.output.iter().enumerate() {
                if out.script_pubkey == self.address.script_pubkey() {
                    let outpoint = OutPoint {
                        txid: tx.compute_txid(),
                        vout: i as u32,
                    };
                    utxos.push((outpoint, out.clone()));
                }
            }

            // Remove the inputs used utxos
            for input in &tx.input {
                if let Some(index) = utxos
                    .iter()
                    .position(|(op, _)| op == &input.previous_output)
                {
                    utxos.remove(index);
                }
            }
        }

        Ok(utxos)
    }

    pub async fn select_utxos_by_target_value(
        &self,
        utxos: &[(OutPoint, TxOut)],
        target_amount: Amount,
    ) -> anyhow::Result<Vec<(OutPoint, TxOut)>> {
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

    pub async fn get_utxos_to_merge(&self) -> anyhow::Result<Vec<(OutPoint, TxOut)>> {
        let mut utxos_to_merge = Vec::new();
        let available_utxos = self.get_available_utxos().await?;

        // If the amount is greater than the minimum amount to merge
        for (outpoint, txout) in available_utxos.iter() {
            if txout.value >= self.minimum_amount {
                utxos_to_merge.push((*outpoint, txout.clone()));
            }
            if utxos_to_merge.len() == self.merge_limit {
                break;
            }
        }
        if utxos_to_merge.len() > 1 {
            return Ok(utxos_to_merge);
        }
        Ok(vec![])
    }

    pub async fn sync_context_with_blockchain(&self) -> anyhow::Result<()> {
        if self.context.read().await.is_empty() {
            return Ok(());
        }

        while let Some(tx) = self.context.write().await.pop_front() {
            let res = self
                .btc_client
                .check_tx_confirmation(&tx.compute_txid(), CTX_REQUIRED_CONFIRMATIONS)
                .await?;

            if !res {
                self.context.write().await.push_front(tx);
                break;
            }
        }
        Ok(())
    }

    pub async fn insert_transaction(&self, tx: Transaction) {
        for ctx_tx in self.context.read().await.iter() {
            if ctx_tx.compute_txid() == tx.compute_txid() {
                return;
            }
        }
        self.context.write().await.push_back(tx);
    }

    pub fn get_btc_client(&self) -> Arc<dyn BitcoinOps> {
        self.btc_client.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use bitcoin::{
        absolute, hashes::Hash, transaction, Network, ScriptBuf, Sequence, TxIn, Txid, Witness,
    };
    use via_btc_client::inscriber::test_utils::{MockBitcoinOps, MockBitcoinOpsConfig};

    use super::*;

    fn bridge_address() -> Address {
        Address::p2pkh(
            bitcoin::PublicKey::from_slice(&[0x02; 33]).unwrap(),
            Network::Bitcoin,
        )
    }

    fn random_address() -> Address {
        Address::p2pkh(
            bitcoin::PublicKey::from_str(
                "0279BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798",
            )
            .unwrap(),
            Network::Bitcoin,
        )
    }

    #[tokio::test]
    async fn test_get_available_utxos() {
        let bridge_address = bridge_address();
        let mut config = MockBitcoinOpsConfig::default();
        let utxos = vec![(
            OutPoint {
                txid: Txid::all_zeros(),
                vout: 0,
            },
            TxOut {
                value: Amount::from_sat(700),
                script_pubkey: bridge_address.script_pubkey(),
            },
        )];
        config.set_utxos(utxos.clone());

        let client = Arc::new(MockBitcoinOps::new(config));
        let manager = UtxoManager::new(client, bridge_address, Amount::ZERO, 100);
        let utxos_out = manager.get_available_utxos().await.unwrap();

        assert_eq!(utxos, utxos_out);
    }

    #[tokio::test]
    async fn test_get_utxos_to_merge_all_gt_minimum_amount() {
        let bridge_address = bridge_address();
        let mut config = MockBitcoinOpsConfig::default();
        let utxos = vec![
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    value: Amount::from_sat(600),
                    script_pubkey: bridge_address.script_pubkey(),
                },
            ),
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    value: Amount::from_sat(500),
                    script_pubkey: bridge_address.script_pubkey(),
                },
            ),
        ];
        config.set_utxos(utxos.clone());

        let client = Arc::new(MockBitcoinOps::new(config));
        let manager = UtxoManager::new(client, bridge_address, Amount::from_sat(500), 100);
        let utxos_out = manager.get_utxos_to_merge().await.unwrap();

        assert_eq!(utxos, utxos_out);
    }

    #[tokio::test]
    async fn test_get_utxos_to_merge_when_merge_limit() {
        let bridge_address = bridge_address();
        let mut config = MockBitcoinOpsConfig::default();
        let utxos = vec![
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    value: Amount::from_sat(600),
                    script_pubkey: bridge_address.script_pubkey(),
                },
            ),
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    value: Amount::from_sat(500),
                    script_pubkey: bridge_address.script_pubkey(),
                },
            ),
        ];
        config.set_utxos(utxos.clone());

        let client = Arc::new(MockBitcoinOps::new(config));
        let merge_limit = 2;
        let manager = UtxoManager::new(client, bridge_address, Amount::from_sat(0), merge_limit);
        let utxos_out = manager.get_utxos_to_merge().await.unwrap();

        assert_eq!(utxos_out.len(), merge_limit);
    }

    #[tokio::test]
    async fn test_get_utxos_to_merge_some_gt_minimum_amount() {
        let bridge_address = bridge_address();
        let mut config = MockBitcoinOpsConfig::default();
        let utxos = vec![
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    value: Amount::from_sat(100),
                    script_pubkey: bridge_address.script_pubkey(),
                },
            ),
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    value: Amount::from_sat(500),
                    script_pubkey: bridge_address.script_pubkey(),
                },
            ),
            (
                OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                TxOut {
                    value: Amount::from_sat(600),
                    script_pubkey: bridge_address.script_pubkey(),
                },
            ),
        ];
        config.set_utxos(utxos.clone());

        let client = Arc::new(MockBitcoinOps::new(config));
        let manager = UtxoManager::new(client, bridge_address, Amount::from_sat(500), 100);
        let utxos_out = manager.get_utxos_to_merge().await.unwrap();
        let expected_utxos = vec![utxos[1].clone(), utxos[2].clone()];
        assert_eq!(expected_utxos, utxos_out);
    }

    #[tokio::test]
    async fn test_get_utxos_to_merge_when_one_utxo_and_lt_minimum_amount() {
        let bridge_address = bridge_address();
        let mut config = MockBitcoinOpsConfig::default();
        let utxos = vec![(
            OutPoint {
                txid: Txid::all_zeros(),
                vout: 0,
            },
            TxOut {
                value: Amount::from_sat(100),
                script_pubkey: bridge_address.script_pubkey(),
            },
        )];
        config.set_utxos(utxos.clone());

        let client = Arc::new(MockBitcoinOps::new(config));
        let manager = UtxoManager::new(client, bridge_address, Amount::from_sat(500), 100);
        let utxos_out = manager.get_utxos_to_merge().await.unwrap();
        let expected_utxos: Vec<(OutPoint, TxOut)> = vec![];
        assert_eq!(expected_utxos, utxos_out);
    }

    #[tokio::test]
    async fn test_chainned_utxos_one_after_one() {
        let bridge_address = bridge_address();
        let mut config = MockBitcoinOpsConfig::default();
        let utxos = vec![(
            OutPoint {
                txid: Txid::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000001",
                )
                .unwrap(),
                vout: 0,
            },
            TxOut {
                value: Amount::from_sat(500),
                script_pubkey: bridge_address.script_pubkey(),
            },
        )];
        config.set_utxos(utxos.clone());
        config.set_tx_confirmation(true);

        let client = Arc::new(MockBitcoinOps::new(config));
        let manager = UtxoManager::new(client, bridge_address.clone(), Amount::from_sat(500), 100);

        //--------------------------------------------------------------------
        // Use the first utxo
        //--------------------------------------------------------------------
        let mut outputs = vec![TxOut {
            script_pubkey: bridge_address.script_pubkey(),
            value: Amount::from_sat(500),
        }];

        let mut unsigned_tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: utxos[0].0,
                script_sig: ScriptBuf::default(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(),
            }],
            output: outputs.clone(),
        };
        let txid1 = unsigned_tx.compute_txid();
        let mut outpoint = OutPoint {
            txid: txid1,
            vout: 0,
        };

        manager.insert_transaction(unsigned_tx.clone()).await;

        let utxos_out = manager.get_available_utxos().await.unwrap();
        let expected_utxos: Vec<(OutPoint, TxOut)> = vec![(outpoint, outputs[0].clone())];
        assert_eq!(expected_utxos, utxos_out);

        //--------------------------------------------------------------------
        // Use the second utxo
        //--------------------------------------------------------------------

        outputs = vec![
            TxOut {
                script_pubkey: random_address().script_pubkey(),
                value: Amount::from_sat(100),
            },
            TxOut {
                script_pubkey: random_address().script_pubkey(),
                value: Amount::from_sat(100),
            },
            TxOut {
                script_pubkey: bridge_address.script_pubkey(),
                value: Amount::from_sat(300),
            },
        ];

        unsigned_tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: expected_utxos[0].0,
                script_sig: ScriptBuf::default(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(),
            }],
            output: outputs.clone(),
        };
        let txid2 = unsigned_tx.compute_txid();

        outpoint = OutPoint {
            txid: unsigned_tx.compute_txid(),
            vout: 2,
        };

        manager.insert_transaction(unsigned_tx.clone()).await;

        let utxos_out = manager.get_available_utxos().await.unwrap();
        let expected_utxos: Vec<(OutPoint, TxOut)> = vec![(outpoint, outputs[2].clone())];
        assert_eq!(expected_utxos, utxos_out);

        let txids = [txid1, txid2];
        assert_eq!(manager.context.read().await.len(), txids.len());

        for (i, tx) in manager.context.read().await.iter().enumerate() {
            assert_eq!(tx.compute_txid(), txids[i]);
        }

        // Sync the context manager with network
        manager.sync_context_with_blockchain().await.unwrap();

        // The context should be empty now
        assert_eq!(manager.context.read().await.len(), 0);
    }
}
