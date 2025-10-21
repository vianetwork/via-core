mod contract;
mod fetching;
mod types;

use clap::Parser;
use tracing::{error, info};
use via_verification::version_27::{
    errors::VerificationError, l1_data_fetcher::L1DataFetcher, verification::verify_snark,
};

use crate::contract::ContractConfig;

#[derive(Debug, Parser)]
#[command(author = "Via", version, about = "Boojum CLI verifier")]
struct Cli {
    /// Batch number to check proof for
    #[arg(long, default_value = "493000")]
    batch: u64,
}

#[tokio::main]
async fn main() -> Result<(), VerificationError> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Cli::parse();

    let batch_number = args.batch;
    let l1_rpc = "https://rpc.ankr.com/eth".to_string();

    info!(
        "Starting Boojum CLI verifier with config: l1_rpc={}; batch #{}",
        l1_rpc, batch_number
    );

    let contract = ContractConfig::new(&l1_rpc)?;

    let (proof, block_number) = contract.get_proof_from_l1(batch_number).await?;

    // Verify the proof
    let verify_resp = verify_snark(&contract, proof, batch_number, block_number).await;

    if let Ok(input) = verify_resp {
        info!("VERIFIED");
        info!("Public input: {}", input);
    } else {
        error!("Failed to verify proof due to an error : {:?}", verify_resp);
    }

    Ok(())
}
