use via_btc_client::{indexer::BitcoinInscriptionIndexer, types::{BitcoinAddress, FullInscriptionMessage}};
use via_verifier_dal::{Connection, Verifier, VerifierDal};
use zksync_types::{ethabi::Address, H256};

use crate::message_processors::{MessageProcessor, MessageProcessorError};
use via_btc_watch_common::utils::normalize_deposit_from_message;

#[derive(Debug)]
pub struct L1ToL2Transaction {
    priority_id: i64,
    tx_id: H256,
    receiver: Address,
    value: i64,
    calldata: Vec<u8>,
    canonical_tx_hash: H256,
}

#[derive(Debug)]
pub struct L1ToL2MessageProcessor {
    bridge_address: BitcoinAddress,
}

impl L1ToL2MessageProcessor {
    pub fn new(bridge_address: BitcoinAddress) -> Self {
        Self { bridge_address }
    }
}

#[async_trait::async_trait]
impl MessageProcessor for L1ToL2MessageProcessor {
    async fn process_messages(
        &mut self,
        storage: &mut Connection<'_, Verifier>,
        msgs: Vec<FullInscriptionMessage>,
        _: &mut BitcoinInscriptionIndexer,
    ) -> Result<bool, MessageProcessorError> {
        let mut priority_ops = Vec::new();

        for msg in msgs {
            if let FullInscriptionMessage::L1ToL2Message(l1_to_l2_msg) = msg {
                if l1_to_l2_msg
                    .tx_outputs
                    .iter()
                    .any(|output| output.script_pubkey == self.bridge_address.script_pubkey())
                {
                    if let Some(dep) = normalize_deposit_from_message(&l1_to_l2_msg)
                        .map_err(|e| MessageProcessorError::Internal(e))?
                    {
                        if storage
                            .via_transactions_dal()
                            .transaction_exists_with_txid(&dep.tx_id)
                            .await
                            .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
                        {
                            tracing::info!(
                                "Transaction with tx_id {} already processed, skipping",
                                dep.tx_id
                            );
                            continue;
                        }

                        priority_ops.push(L1ToL2Transaction {
                            priority_id: dep.priority_id,
                            tx_id: dep.tx_id,
                            receiver: dep.receiver,
                            value: dep.value_sat,
                            calldata: dep.calldata,
                            canonical_tx_hash: dep.canonical_tx_hash,
                        });
                    } else {
                        tracing::warn!("Invalid deposit, l1 tx_id {}", &l1_to_l2_msg.common.tx_id);
                    }
                }
            }
        }

        if priority_ops.is_empty() {
            return Ok(false);
        }

        for new_op in priority_ops {
            storage
                .via_transactions_dal()
                .insert_transaction(
                    new_op.priority_id,
                    new_op.tx_id,
                    new_op.receiver.to_string(),
                    new_op.value,
                    new_op.calldata,
                    new_op.canonical_tx_hash,
                )
                .await
                .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?;
        }

        Ok(true)
    }
}

impl L1ToL2MessageProcessor {}
