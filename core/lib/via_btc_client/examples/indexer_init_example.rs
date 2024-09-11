use anyhow::Result;
use tracing_subscriber;
use via_btc_client::{
    indexer::BitcoinInscriptionIndexer, regtest::BitcoinRegtest, types::BitcoinNetwork,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    tracing::info!("starting Bitcoin client example");
    let context = BitcoinRegtest::new()?;
    let miner = context.get_miner_address()?;
    tracing::info!("miner address: {}", miner);
    let indexer =
        BitcoinInscriptionIndexer::new(&context.get_url(), BitcoinNetwork::Regtest, vec![]).await;

    if let Err(e) = indexer {
        tracing::error!("Failed to create indexer: {:?}", e);
    }
    Ok(())
}
