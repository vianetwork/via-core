use async_trait::async_trait;
use bitcoin::{Address, BlockHash, KnownHrp, Network, Transaction, Txid};
use bitcoincore_rpc::Auth;

mod parser;
use parser::MessageParser;

use crate::{
    client::BitcoinClient,
    traits::BitcoinIndexerOpt,
    types::{BitcoinError, BitcoinIndexerResult, CommonFields, Message},
    BitcoinOps,
};

pub struct BitcoinInscriptionIndexer {
    client: Box<dyn BitcoinOps>,
    parser: MessageParser,
    bridge_address: Option<Address>,
    sequencer_address: Option<Address>,
    verifier_addresses: Vec<Address>,
    starting_block_number: u32,
}

#[async_trait]
impl BitcoinIndexerOpt for BitcoinInscriptionIndexer {
    async fn new(rpc_url: &str, network: Network, txid: &Txid) -> BitcoinIndexerResult<Self>
    where
        Self: Sized,
    {
        let client = Box::new(BitcoinClient::new(rpc_url, network, Auth::None).await?);
        let parser = MessageParser::new(network);
        let tx = client.get_rpc_client().get_transaction(txid).await?;

        let mut init_msgs = parser.parse_transaction(&tx);

        if let Some(Message::SystemBootstrapping(system_bootstrapping)) = init_msgs.pop() {
            Ok(Self {
                client,
                parser,
                bridge_address: Some(system_bootstrapping.input.bridge_p2wpkh_mpc_address),
                verifier_addresses: system_bootstrapping.input.verifier_addresses,
                starting_block_number: system_bootstrapping.input.start_block_height,
                sequencer_address: None,
            })
        } else {
            Err(BitcoinError::Other(
                "Indexer error: provided txid does not contain SystemBootstrapping message"
                    .to_string(),
            ))
        }
    }

    async fn process_blocks(
        &self,
        starting_block: u32,
        ending_block: u32,
    ) -> BitcoinIndexerResult<Vec<Message>> {
        let mut res = Vec::with_capacity((ending_block - starting_block + 1) as usize);
        for block in starting_block..=ending_block {
            res.extend(self.process_block(block).await?);
        }
        Ok(res)
    }

    async fn process_block(&self, block: u32) -> BitcoinIndexerResult<Vec<Message>> {
        if block < self.starting_block_number {
            return Err(BitcoinError::Other(
                "Indexer error: can't get block before starting block".to_string(),
            ));
        }

        let block = self
            .client
            .get_rpc_client()
            .get_block_by_height(block as u128)
            .await?;

        let messages: Vec<Message> = block
            .txdata
            .iter()
            .flat_map(|tx| self.process_tx(tx))
            .filter(|message| self.is_valid_message(message))
            .collect();

        Ok(messages)
    }

    async fn are_blocks_connected(
        &self,
        parent_hash: &BlockHash,
        child_hash: &BlockHash,
    ) -> BitcoinIndexerResult<bool> {
        let child_block = self
            .client
            .get_rpc_client()
            .get_block_by_hash(child_hash)
            .await?;
        Ok(child_block.header.prev_blockhash == *parent_hash)
    }
}

impl BitcoinInscriptionIndexer {
    fn process_tx(&self, tx: &Transaction) -> Vec<Message> {
        self.parser.parse_transaction(tx)
    }

    fn is_valid_message(&self, message: &Message) -> bool {
        match message {
            Message::ProposeSequencer(m) => self
                .verifier_addresses
                .contains(&self.get_sender_address(&m.common)),
            Message::ValidatorAttestation(m) => self
                .verifier_addresses
                .contains(&self.get_sender_address(&m.common)),
            Message::L1BatchDAReference(m) => {
                Some(&self.get_sender_address(&m.common)) == self.sequencer_address.as_ref()
            }
            Message::ProofDAReference(m) => {
                Some(&self.get_sender_address(&m.common)) == self.sequencer_address.as_ref()
            }
            Message::L1ToL2Message(m) => m.amount > bitcoin::Amount::ZERO,
            Message::SystemBootstrapping(_) => true,
        }
    }

    fn get_sender_address(&self, common_fields: &CommonFields) -> Address {
        let public_key =
            secp256k1::XOnlyPublicKey::from_slice(&common_fields.encoded_public_key.as_bytes())
                .map_err(|_| BitcoinError::Other("Invalid public key".to_string()))
                .unwrap();
        Address::p2tr(
            &bitcoin::secp256k1::Secp256k1::new(),
            public_key,
            None,
            KnownHrp::from(self.client.get_network()),
        )
    }
}
