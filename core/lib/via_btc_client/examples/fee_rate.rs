use std::{env, str::FromStr, sync::Arc};

use anyhow::{Context, Result};
use tracing::info;
use via_btc_client::{
    client::BitcoinClient,
    inscriber::Inscriber,
    traits::BitcoinOps,
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
        .with_max_level(tracing::Level::INFO)
        .init();

    let args: Vec<String> = env::args().collect();
    let use_rpc_for_fee_rate = bool::from_str(&args[1].to_string())?;

    let auth = NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string());
    let config = ViaBtcClientConfig {
        network: NETWORK.to_string(),
        external_apis: vec!["https://mempool.space/testnet/api/v1/fees/recommended".into()],
        fee_strategies: vec!["fastestFee".into()],
        use_rpc_for_fee_rate: Some(use_rpc_for_fee_rate),
    };
    let client = Arc::new(BitcoinClient::new(&RPC_URL, auth, config)?);
    let fee_rate = client.get_fee_rate(1).await?;
    info!("Fee rate {:?}", fee_rate);

    Ok(())
}
