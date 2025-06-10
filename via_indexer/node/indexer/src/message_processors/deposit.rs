use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{BitcoinSecp256k1::hashes::Hash, FullInscriptionMessage, L1ToL2Message},
};
use via_indexer_dal::{models::deposit::Deposit, Connection, Indexer, IndexerDal};
use zksync_types::{l1::via_l1::ViaL1Deposit, PriorityOpId, H256};

use crate::message_processors::MessageProcessor;

#[derive(Debug)]
pub struct L1ToL2MessageProcessor {}

impl L1ToL2MessageProcessor {
    pub fn new() -> Self {
        Self {}
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
        let last_priority_id = storage
            .via_transactions_dal()
            .get_last_priority_id()
            .await?;

        let mut next_expected_priority_id = PriorityOpId::from(last_priority_id as u64);

        for msg in msgs {
            if let FullInscriptionMessage::L1ToL2Message(l1_to_l2_msg) = msg {
                let tx_id =
                    H256::from_slice(&l1_to_l2_msg.common.tx_id.as_raw_hash().to_byte_array());

                if storage
                    .via_transactions_dal()
                    .deposit_exists(&l1_to_l2_msg.common.tx_id.to_byte_array())
                    .await?
                {
                    tracing::warn!(
                        "Deposit {} already indexed",
                        l1_to_l2_msg.common.tx_id.to_string()
                    );
                    continue;
                }

                let serial_id = next_expected_priority_id;
                let Some(l1_tx) = self.create_l1_tx_from_message(tx_id, serial_id, &l1_to_l2_msg)
                else {
                    tracing::warn!("Invalid deposit, l1 tx_id {}", &l1_to_l2_msg.common.tx_id);
                    continue;
                };

                deposits.push(l1_tx);

                next_expected_priority_id = next_expected_priority_id.next();
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
        tx_id: H256,
        serial_id: PriorityOpId,
        msg: &L1ToL2Message,
    ) -> Option<Deposit> {
        let deposit = ViaL1Deposit {
            l2_receiver_address: msg.input.receiver_l2_address,
            amount: msg.amount.to_sat(),
            calldata: msg.input.call_data.clone(),
            serial_id,
            l1_block_number: msg.common.block_height as u64,
        };

        if let Some(l1_tx) = deposit.l1_tx() {
            tracing::info!(
                "Created L1 transaction with serial id {:?} (block {}) with deposit amount {} and tx hash {}",
                l1_tx.common_data.serial_id,
                l1_tx.common_data.eth_block,
                deposit.amount,
                l1_tx.common_data.canonical_tx_hash,
            );

            return Some(Deposit {
                priority_id: serial_id.0 as i64,
                tx_id: tx_id.as_bytes().to_vec(),
                receiver: deposit.l2_receiver_address.0.into(),
                block_number: msg.common.block_height,
                value: deposit.amount as i64,
                calldata: deposit.calldata,
                canonical_tx_hash: l1_tx.common_data.canonical_tx_hash.0.into(),
            });
        }
        None
    }
}
