#![cfg(feature = "regtest")]

use anyhow::Result;
use via_btc_client::{client::BitcoinClient, regtest::TestContext, BitcoinOps};

#[tokio::main]
async fn main() -> Result<()> {
    let context = TestContext::setup().await;

    let client = BitcoinClient::new(&context.get_url(), "regtest").await?;

    println!("context.get_url(): {:?}", context.get_url());
    let block_height = client.fetch_block_height().await?;
    println!("Current block height: {}", block_height);

    let estimated_fee = client.get_fee_rate(6).await?;
    println!(
        "Estimated fee for 6 confirmations: {} satoshis/vbyte",
        estimated_fee
    );

    Ok(())
}
