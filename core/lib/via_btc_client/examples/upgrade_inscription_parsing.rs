use std::{str::FromStr, sync::Arc};

use anyhow::Result;
use bitcoin::{Address, Txid};
use via_btc_client::{
    client::BitcoinClient,
    indexer::MessageParser,
    traits::BitcoinOps,
    types::{BitcoinNetwork, NodeAuth, TransactionWithMetadata},
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::via_wallet::SystemWallets;

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

    let txid = Txid::from_str("bb1164dbe54d58336d1e8504451c19ab99a7dab55db186163c427ccb7af4e981")?;
    let mut parser = MessageParser::new(NETWORK);
    let tx = client.get_transaction(&txid).await?;
    let data = parser.parse_bridge_transaction(
        &mut TransactionWithMetadata {
            tx,
            output_vout: None,
            tx_index: 0,
        },
        0,
        &SystemWallets {
            bridge: Address::from_str(
                "bcrt1pm4rre0xv8ryr9lr5lrnzx5tpyk0xr43kfw3aja68c0845vsu5wus3u40fp",
            )?
            .assume_checked(),
            sequencer: Address::from_str("bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56")?
                .assume_checked(),
            governance: Address::from_str("bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56")?
                .assume_checked(),
            verifiers: vec![],
        },
    );
    println!("{:?}", data);

    Ok(())
}
