use via_btc_client::indexer::replay::ScannedDeposit;
use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use via_verifier_dal::{via_transactions_dal::L1ToL2Transaction, Connection, Verifier, VerifierDal};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::METRICS,
};

#[derive(Default, Debug)]
pub struct L1ToL2MessageProcessor {}

#[async_trait::async_trait]
impl MessageProcessor for L1ToL2MessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        for deposit in ScannedDeposit::from_messages(msgs)? {
            let operation = deposit.operation;
            let tx = L1ToL2Transaction {
                priority_id: operation.common_data.serial_id.0 as i64,
                l1_block_number: i64::from(deposit.origin.height),
                tx_id: deposit.txid,
                receiver: format!("{:#x}", deposit.payload.input.0.receiver_l2_address),
                value: deposit.payload.input.1 as i64,
                calldata: deposit.payload.input.0.call_data.clone(),
                canonical_tx_hash: operation.common_data.canonical_tx_hash,
            };
            if storage
                .via_transactions_dal()
                .insert_transaction(tx, &deposit.origin, &deposit.payload)
                .await?
            {
                METRICS.deposit.inc();
            }
        }

        Ok(None)
    }
}
