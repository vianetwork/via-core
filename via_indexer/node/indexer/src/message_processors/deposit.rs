use std::sync::Arc;

use via_btc_client::{
    client::BitcoinClient,
    indexer::BitcoinInscriptionIndexer,
    traits::BitcoinOps,
    types::{
        BitcoinSecp256k1::hashes::hex::{Case, DisplayHex},
        FullInscriptionMessage, L1ToL2Message,
    },
};
use via_indexer_dal::{models::deposit::Deposit, Connection, Indexer, IndexerDal};
use zksync_types::{l1::via_l1::ViaL1Deposit, H256};

use crate::message_processors::MessageProcessor;

#[derive(Debug)]
pub struct L1ToL2MessageProcessor {
    client: Arc<BitcoinClient>,
}

impl L1ToL2MessageProcessor {
    pub fn new(client: Arc<BitcoinClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for L1ToL2MessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Indexer>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> anyhow::Result<()> {
        let mut deposits = Vec::new();

        for msg in msgs {
            if let FullInscriptionMessage::L1ToL2Message(l1_to_l2_msg) = msg {
                let mut tx_id_bytes = l1_to_l2_msg.common.tx_id.as_raw_hash()[..].to_vec();
                tx_id_bytes.reverse();
                let tx_id = H256::from_slice(&tx_id_bytes);

                if storage
                    .via_transactions_dal()
                    .deposit_exists(&tx_id_bytes)
                    .await?
                {
                    tracing::warn!(
                        "Deposit {} already indexed",
                        l1_to_l2_msg.common.tx_id.to_string()
                    );
                    continue;
                }

                let block = self
                    .client
                    .get_block_stats(u64::from(l1_to_l2_msg.common.block_height))
                    .await?;
                let Some(l1_tx) =
                    self.create_l1_tx_from_message(block.time, tx_id, &l1_to_l2_msg)?
                else {
                    tracing::warn!("Invalid deposit, l1 tx_id {}", &l1_to_l2_msg.common.tx_id);
                    continue;
                };

                deposits.push(l1_tx);
            }
        }

        storage
            .via_transactions_dal()
            .insert_deposit_many(deposits)
            .await?;

        Ok(())
    }
}

impl L1ToL2MessageProcessor {
    fn create_l1_tx_from_message(
        &self,
        block_time: u64,
        tx_id: H256,
        msg: &L1ToL2Message,
    ) -> anyhow::Result<Option<Deposit>> {
        let deposit = ViaL1Deposit {
            l2_receiver_address: msg.input.receiver_l2_address.clone(),
            amount: msg.amount.to_sat().clone(),
            calldata: msg.input.call_data.clone(),
            l1_block_number: msg.common.block_height.clone() as u64,
            tx_index: msg
                .common
                .tx_index
                .ok_or_else(|| anyhow::anyhow!("deposit missing tx_index"))?,
            output_vout: msg
                .common
                .output_vout
                .ok_or_else(|| anyhow::anyhow!("deposit missing output_vout"))?,
        };

        if let Some(l1_tx) = deposit.l1_tx() {
            tracing::info!(
                "Created L1 transaction with serial id {:?} (block {}) with deposit amount {} and tx hash {}",
                l1_tx.common_data.serial_id,
                l1_tx.common_data.eth_block,
                deposit.amount,
                l1_tx.common_data.canonical_tx_hash,
            );

            let sender = match msg.common.p2wpkh_address.clone() {
                Some(address) => address.to_string(),
                None => "".into(),
            };

            return Ok(Some(Deposit {
                priority_id: deposit.priority_id().0 as i64,
                tx_id: tx_id.as_bytes().to_vec(),
                sender,
                receiver: format!(
                    "0x{}",
                    deposit.l2_receiver_address.0.to_hex_string(Case::Lower)
                ),
                block_number: msg.common.block_height,
                value: deposit.amount as i64,
                calldata: deposit.calldata,
                canonical_tx_hash: l1_tx.common_data.canonical_tx_hash.0.into(),
                block_timestamp: block_time,
            }));
        }
        Ok(None)
    }
}
