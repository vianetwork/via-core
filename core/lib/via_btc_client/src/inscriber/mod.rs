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
    ) -> Result<types::InscriberOutput> {
        self.update_context().await;



        todo!();
    }

    async fn update_context(&self) {
        todo!();
    }

    async fn prepare_context_for_persistence(&self) {
        todo!();
    }

    async fn rebroadcast_whole_context(&self) {
        todo!();
    }
}

