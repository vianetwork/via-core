use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::BitcoinInscriptionIndexer,
    traits::BitcoinOps,
    types::{BitcoinAddress, BitcoinSecp256k1::hashes::Hash, BitcoinTxid, FullInscriptionMessage},
};
use via_indexer_dal::{
    models::withdraw::{BridgeWithdrawalParam, WithdrawalParam},
    Connection, Indexer, IndexerDal,
};

use crate::message_processors::MessageProcessor;

#[derive(Debug)]
pub struct WithdrawalProcessor {
    bridge_address: BitcoinAddress,
    client: Arc<BitcoinClient>,
}

impl WithdrawalProcessor {
    pub fn new(bridge_address: BitcoinAddress, client: Arc<BitcoinClient>) -> Self {
        Self {
            bridge_address,
            client,
        }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for WithdrawalProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Indexer>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> anyhow::Result<bool> {
        for msg in msgs {
            if let FullInscriptionMessage::BridgeWithdrawal(withdrawal_msg) = msg {
                let mut tx_id_bytes = withdrawal_msg.common.tx_id.as_raw_hash()[..].to_vec();
                tx_id_bytes.reverse();

                if storage
                    .via_transactions_dal()
                    .withdrawal_exists(&tx_id_bytes)
                    .await?
                {
                    tracing::warn!(
                        "Withdrawal {} already indexed",
                        withdrawal_msg.common.tx_id.to_string()
                    );
                    continue;
                }

                let mut total_input = 0;
                for outpoint in withdrawal_msg.input.inputs {
                    let res = self.client.get_transaction(&outpoint.txid).await?;

                    match res.output.get(outpoint.vout as usize) {
                        Some(txout) => {
                            // Verify if the signer is the bridge address.
                            if txout.script_pubkey != self.bridge_address.script_pubkey() {
                                continue;
                            }
                            total_input += txout.value.to_sat()
                        }
                        None => continue,
                    }
                }

                let fee: i64 = (total_input - withdrawal_msg.input.output_amount).try_into()?;

                let mut l1_batch_proof_reveal_tx_id_bytes =
                    withdrawal_msg.input.l1_batch_proof_reveal_tx_id;
                l1_batch_proof_reveal_tx_id_bytes.reverse();

                let mut withdrawals: Vec<WithdrawalParam> = Vec::new();
                let bridge_withdrawal = BridgeWithdrawalParam {
                    vsize: withdrawal_msg.input.v_size,
                    total_size: withdrawal_msg.input.total_size,
                    tx_id: tx_id_bytes,
                    block_number: withdrawal_msg.common.block_height as i64,
                    l1_batch_reveal_tx_id: l1_batch_proof_reveal_tx_id_bytes,
                    fee,
                    withdrawals_count: withdrawal_msg.input.withdrawals.len() as i64,
                };

                for (index, (receiver, amount)) in
                    withdrawal_msg.input.withdrawals.iter().enumerate()
                {
                    withdrawals.push(WithdrawalParam {
                        tx_index: index as i64,
                        receiver: receiver.to_string(),
                        value: *amount,
                    });
                }

                tracing::info!(
                    "New withdrawal found {}",
                    BitcoinTxid::from_slice(&bridge_withdrawal.tx_id)?
                );

                storage
                    .via_transactions_dal()
                    .insert_withdraw(bridge_withdrawal, withdrawals)
                    .await?;
            }
        }

        Ok(true)
    }
}
