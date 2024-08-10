use crate::traits::{BitcoinInscriber, BitcoinOps, BitcoinSigner};
use anyhow::{Context, Result};

use crate::client::BitcoinClient;
use crate::signer::BasicSigner;

mod types;

struct Inscriber {
    client: Box<dyn BitcoinOps>,
    signer: Box<dyn BitcoinSigner>,
    context: types::InscriberContext,
}

// the upper layer call the inscriber in chainable way
// let snapshot = inscriber_instance
//    .inscribe(input)
//    .await?
//    .get_context_snapshot()
//    .await?;
// 
//  persist(snapshot)
impl Inscriber {
    pub async fn new(rpc_url: &str, network: &str, signer_private_key: &str, persisted_ctx: Option<types::InscriberContext> ) -> Result<Self> {
        let client = Box::new(BitcoinClient::new(rpc_url, network).await?);
        let signer = Box::new(BasicSigner::new(signer_private_key)?);
        let context: types::InscriberContext; 
        
        match persisted_ctx {
            Some(ctx) => {
                context = ctx;
            },
            None => {
                context = types::InscriberContext::new();
            }
        }

        Ok(Self { client, signer, context })
    }

    pub async fn inscribe(
        &self,
        input: types::InscriberInput,
    ) -> Result<()> {
        self.sync_context_with_blockchain().await;

        self.prepare_commit_tx_input().await;

        self.prepare_commit_tx_output().await;

        self.sign_commit_tx().await;

        self.prepare_reveal_tx_input().await;

        self.prepare_reveal_tx_output().await;

        self.sign_reveal_tx().await;

        self.broadcast_insription().await;

        self.insert_inscription_to_context().await;

        Ok(())
    }

    async fn sync_context_with_blockchain(&self) {
        todo!();
    }

    async fn prepare_commit_tx_input(&self) {
        todo!();
    }

    async fn prepare_commit_tx_output(&self) {
        todo!();
    }

    async fn sign_commit_tx(&self) {
        todo!();
    }

    async fn prepare_reveal_tx_input(&self) {
        todo!();
    }


    async fn prepare_reveal_tx_output(&self) {
        todo!();
    }

    async fn sign_reveal_tx(&self) {
        todo!();
    }

    async fn broadcast_insription(&self) {
        todo!();
    }

    async fn insert_inscription_to_context(&self) {
        todo!();
    }

    pub async fn get_context_snapshot(&self) {
        todo!();
    }

    pub async fn recreate_context_from_snapshot() {
        todo!();
    }

    async fn rebroadcast_whole_context(&self) {
        todo!();
    }
}

