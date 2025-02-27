use std::{env, str::FromStr};

use anyhow::{Context, Result};
use tracing::info;
use via_da_clients::celestia::client::CelestiaClient;
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::ViaCelestiaConfig;
use zksync_da_client::DataAvailabilityClient;
use zksync_dal::{ConnectionPool, Core, CoreDal};
use zksync_types::{url::SensitiveUrl, L1BatchNumber};

const DEFAULT_DATABASE_URL: &str = "postgres://postgres:notsecurepassword@0.0.0.0:5432/via";
const DEFAULT_CELESTIA: &str = "http://0.0.0.0:26658";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    let home = env::var("VIA_HOME").context("VIA HOME not set")?;
    let _ = dotenv::from_path(home.clone() + "/etc/env/target/via.env");

    let celestia_auth_token = env::var("VIA_CELESTIA_CLIENT_AUTH_TOKEN")?;

    let args: Vec<String> = env::args().collect();
    let block_number = args[1].parse::<u32>().unwrap();
    info!("Fetch withdrawals in block {}", block_number);

    // Connect to db
    let url = SensitiveUrl::from_str(DEFAULT_DATABASE_URL).unwrap();
    let connection_pool = ConnectionPool::<Core>::builder(url, 100)
        .build()
        .await
        .unwrap();
    let l1_batch_number = L1BatchNumber::from(block_number);
    let mut storage = connection_pool.connection().await.unwrap();

    let header_res = storage
        .via_data_availability_dal()
        .get_da_blob(l1_batch_number)
        .await
        .unwrap();
    if header_res.is_none() {
        info!("DA for block not exists yet");
        return Ok(());
    }

    let header = header_res.unwrap();

    let da_config = ViaCelestiaConfig {
        api_node_url: String::from(DEFAULT_CELESTIA),
        auth_token: celestia_auth_token,
        blob_size_limit: 1973786,
    };

    // Connect to withdrawl client
    let client = CelestiaClient::new(da_config).await?;
    let da_client: Box<dyn DataAvailabilityClient> = Box::new(client);
    let withdrawal_client = WithdrawalClient::new(da_client, bitcoin::Network::Regtest);

    let withdrawals = withdrawal_client
        .get_withdrawals(header.blob_id.as_str())
        .await?;

    info!("--------------------------------------------------------");
    info!("Withdrawals {:?}", withdrawals);
    info!("--------------------------------------------------------");

    Ok(())
}
