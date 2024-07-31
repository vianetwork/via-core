#![cfg(feature = "regtest")]

use anyhow::Result;
use via_btc_client::{client::BitcoinClient, regtest::BitcoinRegtest, BitcoinOps};

#[tokio::main]
async fn main() -> Result<()> {
    let context = BitcoinRegtest::new()?;

    println!("Private key: {:?}", context.alice_private_key()?);
    println!("Address: {:?}", context.alice_address()?);

    let client = BitcoinClient::new(&context.get_url(), "regtest").await?;

    let block_height = client.fetch_block_height().await?;
    println!("Current block height: {}", block_height);

    let address = context.alice_address()?;
    let b = client.get_balance(address).await;

    println!("balance : {:?}", b);

    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    let b = client.get_balance(address).await;
    let block_height = client.fetch_block_height().await?;
    println!("Current block height: {}", block_height);
    println!("balance : {:?}", b);

    Ok(())
}
