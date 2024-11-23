// src/main.rs

mod contract;
mod fetching;
mod types;

use clap::Parser;
use tracing::{error, info};
use via_validator::{
    errors::VerificationError,
    l1_data_fetcher::L1DataFetcher,
    proof::L1BatchProof,
    public_inputs::generate_inputs,
    types::{DataJsonOutput, L1BatchAndProofData, VerificationKeyHashJsonOutput},
    utils::check_verification_key,
    verification::verify_snark,
};

use crate::{contract::ContractConfig, fetching::fetch_l1_data};

#[derive(Debug, Parser)]
#[command(author = "Via", version, about = "Boojum CLI verifier")]
struct Cli {
    /// Batch number to check proof for
    #[arg(long, default_value = "493000")]
    batch: u64,
}

#[tokio::main]
async fn main() -> Result<(), VerificationError> {
    // Initialize tracing subscriber for logging
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

    let protocol_version = contract.get_protocol_version(batch_number).await?;
    let protocol_version_id = protocol_version.parse::<u16>().map_err(|_| {
        VerificationError::FetchError("Failed to parse protocol version".to_string())
    })?;

    info!("Protocol version: {}", protocol_version);

    check_verification_key(protocol_version_id).await?;

    let resp = fetch_l1_data(batch_number, protocol_version_id, &l1_rpc).await?;

    let L1BatchAndProofData {
        aux_output,
        mut scheduler_proof,
        batch_l1_data,
        verifier_params: _,
        block_number,
    } = resp.clone();

    let vk_hash = contract.get_verification_key_hash(block_number).await?;

    let snark_vk_scheduler_key_file = format!(
        "keys/protocol_version/{}/scheduler_key.json",
        protocol_version_id
    );

    let inputs = generate_inputs(batch_l1_data.clone());

    scheduler_proof.inputs = inputs.clone();

    let batch_proof = L1BatchProof {
        aggregation_result_coords: aux_output.prepare_aggregation_result_coords(),
        scheduler_proof,
        inputs,
    };

    // Verify the proof
    let verify_resp = verify_snark(
        &snark_vk_scheduler_key_file,
        batch_proof.clone(),
        Some(vk_hash),
    )
    .await;

    if let Ok((input, _, computed_vk_hash)) = verify_resp {
        let mut data = DataJsonOutput::from(resp);
        data.verification_key_hash = VerificationKeyHashJsonOutput {
            layer_1_vk_hash: vk_hash.into(),
            computed_vk_hash: computed_vk_hash.into(),
        };
        data.public_input = input;
        data.is_proof_valid = true;

        info!("VERIFIED");
    } else {
        error!("Failed to verify proof due to an error.");
    }

    Ok(())
}
