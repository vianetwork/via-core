use std::{env, str::FromStr, sync::Arc};

use anyhow::{Context, Result};
use bitcoin::Txid;
use tracing::info;
use via_btc_client::{
    client::BitcoinClient,
    indexer::MessageParser,
    inscriber::Inscriber,
    traits::BitcoinOps,
    types::{
        BitcoinNetwork, InscriptionMessage, NodeAuth, TransactionWithMetadata,
        ValidatorAttestationInput, Vote,
    },
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: BitcoinNetwork = BitcoinNetwork::Regtest;
const TIMEOUT: u64 = 5;

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

    let txid = Txid::from_str("ca3d433c31b5e427b062d60acd3a0b5f8db6793c1aa0c3fab97eec015c72c0f6")?;
    let mut parser = MessageParser::new(NETWORK);
    let tx = client.get_transaction(&txid).await?;
    let data = parser.parse_protocol_upgrade_transactions(
        &TransactionWithMetadata {
            tx,
            output_vout: None,
            tx_index: 0,
        },
        0,
    );
    println!("{:?}", data);

    Ok(())
}
