use std::{
    fs::{remove_file, OpenOptions},
    io::Write,
};

use anyhow::{Context, Result};
use bitcoin::address::NetworkUnchecked;
use tracing::info;
use tracing_subscriber;
use via_btc_client::{
    inscriber::Inscriber,
    types::{
        self as inscribe_types, BitcoinAddress, BitcoinNetwork, InscriptionConfig,
        InscriptionMessage, NodeAuth, ProposeSequencerInput, ValidatorAttestationInput, Vote,
    },
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    const TIMEOUT: u64 = 10;
    let url = "http://0.0.0.0:18443".to_string();
    let prv = "cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R".to_string();
    let addr = "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56"
        .parse::<BitcoinAddress<NetworkUnchecked>>()?;

    let mut inscriber = Inscriber::new(
        &url,
        BitcoinNetwork::Regtest,
        NodeAuth::UserPass("rpcuser".to_string(), "rpcpassword".to_string()),
        &prv,
        None,
    )
    .await
    .context("Failed to create Inscriber")?;

    info!(
        "balance: {}",
        inscriber
            .get_balance()
            .await
            .context("Failed to get balance")?
    );

    /// Bootstrapping message
    let input = inscribe_types::SystemBootstrappingInput {
        start_block_height: 1,
        verifier_p2wpkh_addresses: vec![addr.clone()],
        bridge_p2wpkh_mpc_address: addr.clone(),
    };
    let bootstrap_info = inscriber
        .inscribe(
            InscriptionMessage::SystemBootstrapping(input),
            InscriptionConfig::default(),
        )
        .await?;
    info!(
        "bootstrapping tx sent: {:?}",
        bootstrap_info.final_reveal_tx.txid
    );

    tokio::time::sleep(std::time::Duration::from_secs(TIMEOUT)).await;

    /// Propose sequencer message
    let input = ProposeSequencerInput {
        sequencer_new_p2wpkh_address: addr.clone(),
    };
    let propose_info = inscriber
        .inscribe(
            InscriptionMessage::ProposeSequencer(input),
            InscriptionConfig::default(),
        )
        .await?;
    info!(
        "propose sequencer tx sent : {:?}",
        propose_info.final_reveal_tx.txid
    );

    tokio::time::sleep(std::time::Duration::from_secs(TIMEOUT)).await;

    /// Validator attestation message
    let input = ValidatorAttestationInput {
        reference_txid: propose_info.final_reveal_tx.txid,
        attestation: Vote::Ok,
    };
    let validator_info = inscriber
        .inscribe(
            InscriptionMessage::ValidatorAttestation(input),
            InscriptionConfig::default(),
        )
        .await?;
    info!(
        "validator attestation tx sent : {:?}",
        validator_info.final_reveal_tx.txid
    );

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
    writeln!(file, "{:?}", validator_info.final_reveal_tx.txid)
        .context("Failed to write validator attestation txid")?;

    Ok(())
}
