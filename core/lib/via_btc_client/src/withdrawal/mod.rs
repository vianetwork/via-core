// Withdrawal Builder Service
// This service has main method, that receives a list of bitcoin address and amount to withdraw and also L1batch proofDaReference reveal transaction id
// and then it will use client to get available utxo, and then perform utxo selection based on the total amount of the withdrawal
// and now we know the number of input and output we can estimate the fee and perform final utxo selection
// create a unsigned transaction and return it to the caller

use std::sync::Arc;

use anyhow::Result;
use bitcoin::{
    absolute, transaction, Address, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn,
    TxOut, Txid, Witness,
};
use tracing::{debug, info, instrument};

use crate::{client::BitcoinClient, traits::BitcoinOps, types::BitcoinNetwork};

#[derive(Debug)]
pub struct WithdrawalBuilder {
    client: Arc<dyn BitcoinOps>,
    bridge_address: Address,
}

#[derive(Debug)]
pub struct WithdrawalRequest {
    pub address: Address,
    pub amount: Amount,
}

#[derive(Debug)]
pub struct UnsignedWithdrawalTx {
    pub tx: Transaction,
    pub txid: Txid,
    pub utxos: Vec<(OutPoint, TxOut)>,
    pub change_amount: Amount,
}

impl WithdrawalBuilder {
    #[instrument(skip(rpc_url, auth), target = "bitcoin_withdrawal")]
    pub async fn new(
        rpc_url: &str,
        network: BitcoinNetwork,
        auth: bitcoincore_rpc::Auth,
        bridge_address: Address,
    ) -> Result<Self> {
        info!("Creating new WithdrawalBuilder");
        let client = Arc::new(BitcoinClient::new(rpc_url, network, auth)?);

        Ok(Self {
            client,
            bridge_address,
        })
    }

    #[instrument(skip(self, withdrawals), target = "bitcoin_withdrawal")]
    pub async fn create_unsigned_withdrawal_tx(
        &self,
        withdrawals: Vec<WithdrawalRequest>,
    ) -> Result<UnsignedWithdrawalTx> {
        debug!("Creating unsigned withdrawal transaction");

        // Calculate total amount needed
        let total_amount: Amount = withdrawals
            .iter()
            .try_fold(Amount::ZERO, |acc, w| acc.checked_add(w.amount))
            .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow"))?;

        // Get available UTXOs from bridge address
        let utxos = self.get_available_utxos().await?;

        // Select UTXOs for the withdrawal
        let selected_utxos = self.select_utxos(&utxos, total_amount).await?;

        // Calculate total input amount
        let total_input_amount: Amount = selected_utxos
            .iter()
            .try_fold(Amount::ZERO, |acc, (_, txout)| acc.checked_add(txout.value))
            .ok_or_else(|| anyhow::anyhow!("Input amount overflow"))?;

        // Estimate fee
        let fee_rate = self.client.get_fee_rate(1).await?;
        let fee_amount = self.estimate_fee(
            selected_utxos.len() as u32,
            withdrawals.len() as u32,
            fee_rate,
        )?;

        // Verify we have enough funds
        let total_needed = total_amount
            .checked_add(fee_amount)
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

        // Create outputs for withdrawals
        let mut outputs: Vec<TxOut> = withdrawals
            .into_iter()
            .map(|w| TxOut {
                value: w.amount,
                script_pubkey: w.address.script_pubkey(),
            })
            .collect();

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
            output: outputs,
        };

        let txid = unsigned_tx.compute_txid();

        debug!("Unsigned withdrawal transaction created successfully");

        Ok(UnsignedWithdrawalTx {
            tx: unsigned_tx,
            txid,
            utxos: selected_utxos,
            change_amount,
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
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::Network;

    use super::*;

    #[tokio::test]
    #[ignore] // Remove this to run against a local Bitcoin node
    async fn test_withdrawal_builder() -> Result<()> {
        let network = Network::Regtest;
        let bridge_address = Address::from_str("bcrt1q6rhpng9evgu7ul6k0kr8f8sdre3hy9ym8t7g5h")?
            .require_network(network)?;

        let builder = WithdrawalBuilder::new(
            "http://localhost:18443",
            BitcoinNetwork::Regtest,
            bitcoincore_rpc::Auth::None,
            bridge_address,
        )
        .await?;

        let withdrawals = vec![
            WithdrawalRequest {
                address: Address::from_str("bcrt1qg3y8889zzz0qvg3xhm4e9j8z9wp7524fn0qxya")?
                    .require_network(network)?,
                amount: Amount::from_btc(0.1)?,
            },
            WithdrawalRequest {
                address: Address::from_str("bcrt1qf6em9yq7h8zmmwl9q8ue73x34dj9ql9xl7t4ph")?
                    .require_network(network)?,
                amount: Amount::from_btc(0.2)?,
            },
        ];

        let withdrawal_tx = builder.create_unsigned_withdrawal_tx(withdrawals).await?;
        assert!(!withdrawal_tx.utxos.is_empty());

        Ok(())
    }
}
