use std::sync::Arc;

use anyhow::Result;
use tracing_subscriber;
use via_btc_client::{
    client::BitcoinClient,
    indexer::BitcoinInscriptionIndexer,
    regtest::BitcoinRegtest,
    types::{BitcoinNetwork, NodeAuth},
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: BitcoinNetwork = BitcoinNetwork::Regtest;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    tracing::info!("starting Bitcoin client example");
    let context = BitcoinRegtest::new()?;
    let miner = context.get_miner_address()?;
    tracing::info!("miner address: {}", miner);

    let auth = NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string());
    let config = ViaBtcClientConfig {
        network: NETWORK.to_string(),
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };
    let client = Arc::new(BitcoinClient::new(RPC_URL, auth, config.clone())?);

    let indexer = BitcoinInscriptionIndexer::new(client, config, vec![]).await;

    if let Err(e) = indexer {
        tracing::error!("Failed to create indexer: {:?}", e);
    }
    Ok(())
}
