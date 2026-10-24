use via_btc_client::indexer::replay::ScannedDeposit;
use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use zksync_dal::{Connection, Core, CoreDal};

use crate::{
    message_processors::{MessageProcessor, MessageProcessorError},
    metrics::METRICS,
};

#[derive(Debug, Default)]
pub struct L1ToL2MessageProcessor {}

#[async_trait::async_trait]
impl MessageProcessor for L1ToL2MessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        for deposit in ScannedDeposit::from_messages(msgs)? {
            if storage
                .via_transactions_dal()
                .insert_transaction_l1(
                    &deposit.operation,
                    deposit.operation.eth_block(),
                    deposit.txid,
                    &deposit.origin,
                    &deposit.payload,
                )
                .await?
            {
                METRICS.deposit.inc();
            }
        }

        Ok(None)
    }
}
