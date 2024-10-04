use anyhow::{Context, Result};
use bitcoin::address::NetworkUnchecked;
use std::{
    fs::{remove_file, OpenOptions},
    io::Write,
};
use tracing::info;
use tracing_subscriber;
use via_btc_client::{
    inscriber::Inscriber,
    types::{
        BitcoinAddress, BitcoinNetwork, InscriptionConfig, InscriptionMessage, NodeAuth,
        ProposeSequencerInput, SystemBootstrappingInput, ValidatorAttestationInput, Vote,
    },
};
use zksync_basic_types::H256;

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const TIMEOUT: u64 = 5;

async fn create_inscriber(signer_private_key: &str) -> Result<Inscriber> {
    Inscriber::new(
        RPC_URL,
        BitcoinNetwork::Regtest,
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

    let sequencer_p2wpkh_address = "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56"
        .parse::<BitcoinAddress<NetworkUnchecked>>()?;
    let verifier_1_p2wpkh_address = "bcrt1qw2mvkvm6alfhe86yf328kgvr7mupdx4vln7kpv"
        .parse::<BitcoinAddress<NetworkUnchecked>>()?;
    let verifier_2_p2wpkh_address = "bcrt1qk8mkhrmgtq24nylzyzejznfzws6d98g4kmuuh4"
        .parse::<BitcoinAddress<NetworkUnchecked>>()?;
    let verifier_3_p2wpkh_address = "bcrt1q23lgaa90s85jvtl6dsrkvn0g949cwjkwuyzwdm"
        .parse::<BitcoinAddress<NetworkUnchecked>>()?;
    let bridge_p2wpkh_mpc_address = "bcrt1qdrzjq2mwlhrnhan94em5sl032zd95m73ud8ddw"
        .parse::<BitcoinAddress<NetworkUnchecked>>()?;

    let mut verifier_inscribers: Vec<Inscriber> = vec![
        create_inscriber(&verifier_1_private_key).await?,
        create_inscriber(&verifier_2_private_key).await?,
        create_inscriber(&verifier_3_private_key).await?,
    ];

    // Bootstrapping message
    let input = SystemBootstrappingInput {
        start_block_height: 1,
        verifier_p2wpkh_addresses: vec![
            verifier_1_p2wpkh_address,
            verifier_2_p2wpkh_address,
            verifier_3_p2wpkh_address,
        ],
        bridge_p2wpkh_mpc_address,
        bootloader_hash: H256::zero(),
        abstract_account_hash: H256::random(),
    };
    let bootstrap_info = verifier_inscribers[0]
        .inscribe(
            InscriptionMessage::SystemBootstrapping(input),
            InscriptionConfig::default(),
        )
        .await?;
    info!(
        "Bootstrapping tx sent: {:?}",
        bootstrap_info.final_reveal_tx.txid
    );

    tokio::time::sleep(std::time::Duration::from_secs(TIMEOUT)).await;

    // Propose sequencer message
    let input = ProposeSequencerInput {
        sequencer_new_p2wpkh_address: sequencer_p2wpkh_address,
    };
    let propose_info = verifier_inscribers[1]
        .inscribe(
            InscriptionMessage::ProposeSequencer(input),
            InscriptionConfig::default(),
        )
        .await?;
    info!(
        "Propose sequencer tx sent: {:?}",
        propose_info.final_reveal_tx.txid
    );

    tokio::time::sleep(std::time::Duration::from_secs(TIMEOUT)).await;

    // Validator attestation messages for proposed sequencer
    let verifier_inscribers_len = verifier_inscribers.len();
    let mut validators_info = Vec::with_capacity(verifier_inscribers_len);
    let input = ValidatorAttestationInput {
        reference_txid: propose_info.final_reveal_tx.txid,
        attestation: Vote::Ok,
    };

    for (i, inscriber) in verifier_inscribers.iter_mut().enumerate() {
        let validator_info = inscriber
            .inscribe(
                InscriptionMessage::ValidatorAttestation(input.clone()),
                InscriptionConfig::default(),
            )
            .await?;
        info!(
            "Validator {} attestation tx sent: {:?}",
            i + 1,
            validator_info.final_reveal_tx.txid
        );

        validators_info.push(validator_info);

        if i < verifier_inscribers_len - 1 {
            tokio::time::sleep(std::time::Duration::from_secs(TIMEOUT)).await;
        }
    }

    if let Err(err) = remove_file("txids.via") {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(anyhow::anyhow!(
                "Failed to delete existing txids.via file: {:?}",
                err
            ));
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("txids.via")
        .context("Failed to open txids.via file")?;

    writeln!(file, "{:?}", bootstrap_info.final_reveal_tx.txid)
        .context("Failed to write bootstrapping txid")?;
    writeln!(file, "{:?}", propose_info.final_reveal_tx.txid)
        .context("Failed to write propose sequencer txid")?;
    for validator_info in validators_info {
        writeln!(file, "{:?}", validator_info.final_reveal_tx.txid)
            .context("Failed to write validator attestation txid")?;
    }

    Ok(())
}
