use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::BitcoinInscriptionIndexer,
    traits::BitcoinOps,
    types::{
        BitcoinSecp256k1::hashes::{
            hex::{Case, DisplayHex},
            Hash,
        },
        FullInscriptionMessage,
    },
};
use via_indexer_dal::{models::withdraw::Withdrawal, Connection, Indexer, IndexerDal};
use via_verifier_types::withdrawal::get_withdrawal_requests;

use crate::message_processors::MessageProcessor;

#[derive(Debug)]
pub struct WithdrawalProcessor {
    client: Arc<BitcoinClient>,
}

impl WithdrawalProcessor {
    pub fn new(client: Arc<BitcoinClient>) -> Self {
        Self { client }
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
                let tx_id = withdrawal_msg.common.tx_id.as_byte_array().to_vec();
                let withdrawals = get_withdrawal_requests(withdrawal_msg.input.withdrawals);

                tracing::info!(
                    "New bridge withdrawal found: hash: {}, count: {}",
                    tx_id.to_hex_string(Case::Lower),
                    withdrawals.len()
                );

                let block = self
                    .client
                    .get_block_stats(withdrawal_msg.common.block_height as u64)
                    .await?;

                if !withdrawals.is_empty() {
                    let mut transaction = storage.start_transaction().await?;
                    for w in withdrawals {
                        transaction
                            .via_transactions_dal()
                            .insert_withdraw(Withdrawal {
                                id: w.id,
                                tx_id: tx_id.clone(),
                                l2_tx_log_index: w.l2_tx_log_index as i64,
                                block_number: withdrawal_msg.common.block_height as i64,
                                receiver: w.receiver.to_string(),
                                value: w.amount.to_sat() as i64,
                                timestamp: block.time as i64,
                            })
                            .await?;
                    }

                    transaction.commit().await?;
                }

                tracing::info!(
                    "Bridge withdrawal {} inserted",
                    tx_id.to_hex_string(Case::Lower),
                );
            }
        }

        Ok(true)
    }
}
