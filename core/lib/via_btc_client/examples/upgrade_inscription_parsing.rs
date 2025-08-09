use std::{str::FromStr, sync::Arc};

use anyhow::Result;
use bitcoin::Txid;
use via_btc_client::{
    client::BitcoinClient,
    indexer::MessageParser,
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

    let auth = NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string());
    let config = ViaBtcClientConfig {
        network: NETWORK.to_string(),
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };

    let client = Arc::new(BitcoinClient::new(RPC_URL, auth, config)?);

    let txid = Txid::from_str("87bf03c6377bc6724e4022fa0a884dbe9cc811cd3db8d5840a042a27f78dce91")?;
    let mut parser = MessageParser::new(NETWORK);
    let tx = client.get_transaction(&txid).await?;
    let data = parser.parse_system_transaction(&tx, 0, None);
    println!("{:?}", data);

    Ok(())
}
