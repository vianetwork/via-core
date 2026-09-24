use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::FullInscriptionMessage};
use via_verifier_dal::{Connection, Verifier, VerifierDal};
use via_verifier_types::withdrawal_observation::{ObservedWithdrawal, WithdrawalInclusion, WithdrawalObservation};

use super::{MessageProcessor, MessageProcessorError};

#[derive(Default, Debug)]
pub struct WithdrawalProcessor;

#[async_trait::async_trait]
impl MessageProcessor for WithdrawalProcessor {
    async fn process_messages(
        &mut self, storage: &mut Connection<'_, Verifier>, msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<Option<u32>, MessageProcessorError> {
        for msg in msgs {
            if let FullInscriptionMessage::BridgeWithdrawal(message) = msg {
                let wallet = message
                    .input
                    .bridge_script_pubkey
                    .ok_or_else(|| anyhow::anyhow!("Missing verified withdrawal wallet context"))?;
                let block_hash =
                    message.input.block_hash.ok_or_else(|| anyhow::anyhow!("Missing withdrawal block inclusion"))?;
                let observation = WithdrawalObservation {
                    transaction: message.input.transaction,
                    prevouts: message.input.prevouts,
                    withdrawals: message
                        .input
                        .withdrawals
                        .into_iter()
                        .map(|payment| ObservedWithdrawal {
                            vout: payment.vout,
                            reference: payment.l2_meta.l2_id,
                            script_pubkey: payment.receiver.script_pubkey(),
                            amount: payment.value,
                        })
                        .collect(),
                    inclusion: Some(WithdrawalInclusion { block_hash, block_height: message.common.block_height }),
                };
                storage.via_withdrawal_dal().record_withdrawal_observation(wallet.as_bytes(), &observation).await?;
            }
        }
        Ok(None)
    }
}
