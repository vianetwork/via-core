use std::{env, str::FromStr};

use anyhow::{Context, Result};
use bitcoin::Txid;
use tracing::info;
use via_btc_client::{
    inscriber::Inscriber,
    types::{BitcoinNetwork, InscriptionMessage, NodeAuth, ValidatorAttestationInput, Vote},
};

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: BitcoinNetwork = BitcoinNetwork::Regtest;
const TIMEOUT: u64 = 5;

async fn create_inscriber(signer_private_key: &str) -> Result<Inscriber> {
    Inscriber::new(
        RPC_URL,
        NETWORK,
        NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string()),
        signer_private_key,
        None,
    )
    .await
    .context("Failed to create Inscriber")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // Regtest verifier keys
    let verifier_1_private_key = "cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm".to_string();
    let verifier_2_private_key = "cQ4UHjdsGWFMcQ8zXcaSr7m4Kxq9x7g9EKqguTaFH7fA34mZAnqW".to_string();
    let verifier_3_private_key = "cS9UbUKKepDjthBFPBDBe5vGVjNXXygCN75kPWmNKk7HTPV8p6he".to_string();

    let mut verifier_inscribers: Vec<Inscriber> = vec![
        create_inscriber(&verifier_1_private_key).await?,
        create_inscriber(&verifier_2_private_key).await?,
        create_inscriber(&verifier_3_private_key).await?,
    ];

    let args: Vec<String> = env::args().collect();
    let l1_batch_proof_ref_final_reveal_txid = Txid::from_str(&args[1]).unwrap();
    info!(
        "Verifying L1 batch with proof reveal tx id: {}...",
        l1_batch_proof_ref_final_reveal_txid
    );

    // Validator attestation messages for L1 batch
    let verifier_inscribers_len = verifier_inscribers.len();

    let input = ValidatorAttestationInput {
        reference_txid: l1_batch_proof_ref_final_reveal_txid,
        attestation: Vote::Ok,
    };

    for (i, inscriber) in verifier_inscribers.iter_mut().enumerate() {
        let validator_info = inscriber
            .inscribe(InscriptionMessage::ValidatorAttestation(input.clone()))
            .await?;
        info!(
            "Validator {} attestation tx sent: {:?}",
            i + 1,
            validator_info.final_reveal_tx.txid
        );

        if i < verifier_inscribers_len - 1 {
            tokio::time::sleep(std::time::Duration::from_secs(TIMEOUT)).await;
        }
    }

    Ok(())
}
