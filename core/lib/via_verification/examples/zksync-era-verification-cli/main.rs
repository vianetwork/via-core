mod contract;
mod fetching;
mod types;

use clap::Parser;
use tracing::{error, info};
use via_verification::{
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

    let vk_hash = contract.get_verification_key_hash(block_number).await?;

    let protocol_version = contract.get_protocol_version(batch_number).await?;

    // Verify the proof
    let verify_resp = verify_snark(&protocol_version, proof, Some(vk_hash)).await;

    if let Ok((input, computed_vk_hash)) = verify_resp {
        info!("VERIFIED");
        info!("Public input: {}", input);
        info!("Computed VK hash: {}", computed_vk_hash);
    } else {
        error!("Failed to verify proof due to an error : {:?}", verify_resp);
    }

    Ok(())
}
