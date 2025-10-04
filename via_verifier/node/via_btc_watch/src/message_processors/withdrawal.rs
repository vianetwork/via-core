use via_btc_client::{
    indexer::BitcoinInscriptionIndexer,
    types::{
        BitcoinSecp256k1::hashes::{
            hex::{Case, DisplayHex},
            Hash,
        },
        FullInscriptionMessage,
    },
};
use via_verifier_dal::{Connection, Verifier, VerifierDal};
use via_verifier_types::withdrawal::get_withdrawal_requests;

use super::{MessageProcessor, MessageProcessorError};
use crate::metrics::METRICS;

#[derive(Debug)]
pub struct WithdrawalProcessor;

impl WithdrawalProcessor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl MessageProcessor for WithdrawalProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<bool, MessageProcessorError> {
        for msg in msgs {
            if let FullInscriptionMessage::BridgeWithdrawal(withdrawal_msg) = msg {
                tracing::info!("Processing withdrawal bridge transaction...");

                let tx_id = withdrawal_msg.common.tx_id.as_byte_array().to_vec();
                let withdrawals = get_withdrawal_requests(withdrawal_msg.input.withdrawals);

                let id_opt = storage
                    .via_withdrawal_dal()
                    .get_bridge_withdrawal_id(&tx_id)
                    .await?;

                let mut transaction = storage.start_transaction().await?;

                let id = match id_opt {
                    Some(id) => id,
                    None => {
                        transaction
                            .via_withdrawal_dal()
                            .insert_bridge_withdrawal_tx(&tx_id)
                            .await?
                    }
                };

                transaction
                    .via_withdrawal_dal()
                    .mark_bridge_withdrawal_tx_as_processed(&tx_id)
                    .await?;

                transaction
                    .via_withdrawal_dal()
                    .insert_withdrawals(&withdrawals)
                    .await?;

                transaction
                    .via_withdrawal_dal()
                    .mark_withdrawals_as_processed(id, &withdrawals)
                    .await?;

                transaction.commit().await?;

                tracing::info!(
                    "Bridge withdrawal {} indexed",
                    tx_id.to_hex_string(Case::Lower)
                );

                METRICS.number_withdrawals_processed.set(withdrawals.len());
            }
        }

        Ok(true)
    }
}
