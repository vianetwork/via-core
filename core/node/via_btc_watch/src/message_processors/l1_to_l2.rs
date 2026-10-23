use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{FullInscriptionMessage, L1ToL2Message},
};
use zksync_dal::{Connection, Core, CoreDal};
use zksync_types::{
    l1::{via_l1::ViaL1Deposit, L1Tx},
    H256,
};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
};

#[derive(Debug, Default)]
pub struct L1ToL2MessageProcessor {}

// jscpd:ignore-start Distinct deposit queue / execution representations require separate concrete transaction adapters.
#[async_trait::async_trait]
impl MessageProcessor for L1ToL2MessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        for message in msgs {
            if let FullInscriptionMessage::L1ToL2Message(deposit) = &message {
                let payload = via_btc_client::indexer::replay::semantic_payload(&message)
                    .map_err(anyhow::Error::from)?;
                let inclusion = deposit.common.inclusion().map_err(anyhow::Error::from)?;
                let vout = u32::try_from(
                    deposit
                        .common
                        .output_vout
                        .ok_or_else(|| anyhow::anyhow!("deposit missing outpoint"))?,
                )
                .map_err(anyhow::Error::from)?;
                let mut tx_id_bytes = deposit.common.tx_id.as_raw_hash()[..].to_vec();
                tx_id_bytes.reverse();
                let tx_id = H256::from_slice(&tx_id_bytes);
                let Some(operation) = self.create_l1_tx_from_message(deposit)? else {
                    continue;
                };
                storage
                    .via_transactions_dal()
                    .record_deposit_evidence(
                        inclusion.carrier_txid,
                        vout,
                        &payload,
                        inclusion,
                        operation.hash(),
                    )
                    .await?;
                storage
                    .via_transactions_dal()
                    .insert_transaction_l1(&operation, operation.eth_block(), tx_id)
                    .await?;
            }
        }

        Ok(None)
    }
}
// jscpd:ignore-end

impl L1ToL2MessageProcessor {
    fn create_l1_tx_from_message(&self, msg: &L1ToL2Message) -> Result<Option<L1Tx>, MessageProcessorError> {
        let deposit = ViaL1Deposit {
            l2_receiver_address: msg.input.receiver_l2_address,
            amount: msg.amount.to_sat(),
            calldata: msg.input.call_data.clone(),
            l1_block_number: msg.common.block_height as u64,
            tx_index: msg
                .common
                .tx_index
                .ok_or_else(|| MessageProcessorError::Internal(anyhow::anyhow!("deposit missing tx_index")))?,
            output_vout: msg
                .common
                .output_vout
                .ok_or_else(|| MessageProcessorError::Internal(anyhow::anyhow!("deposit missing output_vout")))?,
        };

        if let Some(l1_tx) = deposit.l1_tx() {
            tracing::info!(
                "Created L1 transaction with serial id {:?} (block {}) with deposit amount {} and tx hash {}",
                l1_tx.common_data.serial_id,
                l1_tx.common_data.eth_block,
                deposit.amount,
                l1_tx.common_data.canonical_tx_hash,
            );
            return Ok(Some(l1_tx));
        }
        Ok(None)
    }
}
