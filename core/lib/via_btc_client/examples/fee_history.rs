use std::{env, str::FromStr};

use anyhow::{Context, Result};
use tracing::info;
use via_btc_client::{
    inscriber::Inscriber,
    types::{BitcoinNetwork, NodeAuth},
};

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: BitcoinNetwork = BitcoinNetwork::Regtest;
const PK: &str = "cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args: Vec<String> = env::args().collect();
    let number_blocks = usize::from_str(&args[1].to_string())?;

    let inscriber = Inscriber::new(
        RPC_URL,
        NETWORK,
        NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string()),
        PK,
        None,
    )
    .await
    .context("Failed to Inscriber")?;

    let client = inscriber.get_client().await;

    let to_block = client.fetch_block_height().await? as usize;
    let from_block = to_block - number_blocks;
    info!(
        "Fetch blocks fee history from block {} to {}",
        from_block, to_block
    );

    let fee_history = client.get_fee_history(from_block, to_block).await?;

    info!("Fee history {:?}", fee_history);

    Ok(())
}
