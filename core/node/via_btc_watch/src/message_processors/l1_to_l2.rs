use via_btc_client::types::{BitcoinAddress, FullInscriptionMessage};
use zksync_dal::{Connection, Core};

use crate::message_processors::{MessageProcessor, MessageProcessorError};

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
        _storage: &mut Connection<'_, Core>,
        msgs: Vec<FullInscriptionMessage>,
    ) -> Result<(), MessageProcessorError> {
        for msg in msgs {
            if let FullInscriptionMessage::L1ToL2Message(l1_to_l2_msg) = msg {
                tracing::debug!("Processing L1 to L2 message: {:?}", l1_to_l2_msg);

                if l1_to_l2_msg
                    .tx_outputs
                    .iter()
                    .any(|output| output.script_pubkey == self.bridge_address.script_pubkey())
                {
                    // insert l2 & l1 transactions
                    // storage
                    //     .transactions_dal()
                    //     .insert_transaction_l2()
                    //     .await?;
                    // storage
                    //     .transactions_dal()
                    //     .insert_transaction_l1()
                    //     .await?;

                    let _l2_address = &l1_to_l2_msg.input.receiver_l2_address.0;
                    let _contract_address = &l1_to_l2_msg.input.l2_contract_address.0;
                    let _amount = l1_to_l2_msg.amount.to_sat();

                    // Store the L1 to L2 message in the database
                    // storage
                    //     .transactions_dal()
                    //     .store_l1_to_l2_message()
                    //     .await?;
                }
            }
        }
        Ok(())
    }
}
