use std::{
    collections::HashMap,
    env, fmt,
    fs::{create_dir_all, remove_dir_all, File},
    io::Write,
    path::Path,
    str::FromStr,
    sync::Arc,
};

use anyhow::Context;
use bitcoin::{address::NetworkUnchecked, CompressedPublicKey, Txid};
use musig2::KeyAggContext;
use secp256k1_musig2::PublicKey as Musig2PublicKey;
use serde::{Deserialize, Serialize};
use serde_json::to_string_pretty;
use tracing::info;
use via_btc_client::{
    client::BitcoinClient,
    indexer::MessageParser,
    inscriber::Inscriber,
    types::{
        BitcoinAddress, BitcoinNetwork, FullInscriptionMessage, InscriptionMessage, NodeAuth,
        ProposeSequencerInput, SystemBootstrappingInput, ValidatorAttestationInput, Vote,
    },
};
use zksync_basic_types::H256;
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum InscriptionType {
    SystemBootstrapping,
    Attest,
    Empty,
}

impl fmt::Display for InscriptionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = match self {
            Self::SystemBootstrapping => "SystemBootstrapping",
            Self::Attest => "Attest",
            Self::Empty => "",
        };
        write!(f, "{}", t)
    }
}

impl From<&str> for InscriptionType {
    fn from(value: &str) -> Self {
        match value {
            "SystemBootstrapping" => Self::SystemBootstrapping,
            "Attest" => Self::Attest,
            _ => Self::Empty,
        }
    }
}

async fn create_inscriber(
    signer_private_key: &str,
    rpc_url: &str,
    rpc_username: &str,
    rpc_password: &str,
    network: BitcoinNetwork,
) -> anyhow::Result<Inscriber> {
    let auth = NodeAuth::UserPass(rpc_username.to_string(), rpc_password.to_string());
    let config = ViaBtcClientConfig {
        network: network.to_string(),
        external_apis: vec![format!("https://mempool.space/testnet/api/v1/fees/recommended")],
        fee_strategies: vec![format!("fastestFee")],
        use_rpc_for_fee_rate: None,
    };
    let client = Arc::new(BitcoinClient::new(rpc_url, auth, config)?);
    Inscriber::new(client, signer_private_key, None)
        .await
        .context("Failed to create Inscriber")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args: Vec<String> = env::args().collect();
    let network: BitcoinNetwork = args[1].parse().expect("Invalid network value");
    let rpc_url = args[2].clone();
    let rpc_username = args[3].clone();
    let rpc_password = args[4].clone();
    let inscription = InscriptionType::from(args[5].clone().as_str());
    let private_key = args[6].clone();

    let mut inscriber = create_inscriber(
        &private_key,
        &rpc_url,
        &rpc_username,
        &rpc_password,
        network,
    )
    .await?;

    match inscription {
        InscriptionType::SystemBootstrapping => {
            let system_tx_id = bootstrap_inscription(&args, &mut inscriber, network).await?;
            let propose_sequencer_tx_id = propose_sequencer(&args, &mut inscriber, network).await?;
            let mut data = HashMap::new();
            data.insert("system_tx_id".to_string(), system_tx_id.to_string());
            data.insert(
                "propose_sequencer_tx_id".to_string(),
                propose_sequencer_tx_id.to_string(),
            );
            data.insert(
                "tx_type".to_string(),
                InscriptionType::SystemBootstrapping.to_string(),
            );
            let json_data = to_string_pretty(&data).expect("Failed to serialize");

            let dir = format!("etc/env/via/genesis/{}", network,);

            let folder_path = Path::new(&dir);
            if network == BitcoinNetwork::Regtest && folder_path.exists() {
                remove_dir_all(folder_path)?;
            }

            create_dir_all(dir.clone())?;

            let path = format!("{}/{}.json", dir, InscriptionType::SystemBootstrapping);

            save_inscription_metadata(json_data.clone(), path)?;
        }
        InscriptionType::Attest => {
            let tx_id = attest_sequencer(&args, &mut inscriber, network).await?;

            let mut data = HashMap::new();
            data.insert("tx_id".to_string(), tx_id.to_string());
            data.insert("tx_type".to_string(), InscriptionType::Attest.to_string());
            let json_data = to_string_pretty(&data).expect("Failed to serialize");

            let dir = format!("etc/env/via/genesis/{}", network,);

            create_dir_all(dir.clone())?;

            let path = format!("{}/{}_{}.json", dir, InscriptionType::Attest, tx_id);

            save_inscription_metadata(json_data, path)?;
        }
        InscriptionType::Empty => {
            anyhow::bail!("Invalid inscription")
        }
    }

    Ok(())
}

/// Create a system bootstraping inscription
pub async fn bootstrap_inscription(
    args: &Vec<String>,
    inscriber: &mut Inscriber,
    network: BitcoinNetwork,
) -> anyhow::Result<Txid> {
    let start_block_height = args[7].clone().parse::<u32>()?;
    let verifier_public_keys = args[8]
        .clone()
        .split(",")
        .map(|pub_key_str| {
            let pub_key = Musig2PublicKey::from_str(pub_key_str)?;
            Ok(pub_key)
        })
        .collect::<anyhow::Result<Vec<Musig2PublicKey>>>()?;

    let verifier_p2wpkh_addresses = args[8]
        .clone()
        .split(",")
        .map(|pub_key_str| {
            let cpk = CompressedPublicKey::from_str(pub_key_str)?;
            let address = BitcoinAddress::p2wpkh(&cpk, network).as_unchecked().clone();
            Ok(address)
        })
        .collect::<anyhow::Result<Vec<BitcoinAddress<NetworkUnchecked>>>>()?;

    let bootloader_hash = H256::from_str(&args[9].clone())?;
    let abstract_account_hash = H256::from_str(&args[10].clone())?;
    let governance_address = BitcoinAddress::from_str(&args[11].clone())?
        .require_network(network)?
        .as_unchecked()
        .clone();

    let bridge_musig2_address = BitcoinAddress::from_str(&args[12].clone())?
        .require_network(network)?
        .as_unchecked()
        .clone();

    let computed_bridge_musig2_address = compute_bridge_address(verifier_public_keys, network)?
        .as_unchecked()
        .clone();

    if bridge_musig2_address != computed_bridge_musig2_address {
        anyhow::bail!(
            "Bridge address mismatch: expected {:?}, got {:?}",
            bridge_musig2_address,
            computed_bridge_musig2_address
        );
    }
    // Bootstrapping message
    let input = SystemBootstrappingInput {
        start_block_height,
        verifier_p2wpkh_addresses,
        bridge_musig2_address,
        bootloader_hash,
        abstract_account_hash,
        governance_address,
    };

    let bootstrap_info = inscriber
        .inscribe(InscriptionMessage::SystemBootstrapping(input))
        .await?;
    info!(
        "Bootstrapping tx sent: {:?}",
        &bootstrap_info.final_reveal_tx.txid
    );

    Ok(bootstrap_info.final_reveal_tx.txid)
}

/// Propose sequencer message
pub async fn propose_sequencer(
    args: &Vec<String>,
    inscriber: &mut Inscriber,
    network: BitcoinNetwork,
) -> anyhow::Result<Txid> {
    let sequencer_new_p2wpkh_address = BitcoinAddress::from_str(&args[13].clone())?
        .require_network(network)?
        .as_unchecked()
        .clone();

    let input = ProposeSequencerInput {
        sequencer_new_p2wpkh_address,
    };

    let propose_info = inscriber
        .inscribe(InscriptionMessage::ProposeSequencer(input))
        .await?;
    info!(
        "Propose sequencer tx sent: {:?}",
        &propose_info.final_reveal_tx.txid
    );

    Ok(propose_info.final_reveal_tx.txid)
}

/// Attest the sequencer proposal
pub async fn attest_sequencer(
    args: &Vec<String>,
    inscriber: &mut Inscriber,
    network: BitcoinNetwork,
) -> anyhow::Result<Txid> {
    let propose_sequencer_reveal_tx_id = Txid::from_str(&args[7].clone())?;

    let client = inscriber.get_client().await;
    let tx = client
        .get_transaction(&propose_sequencer_reveal_tx_id)
        .await?;
    let mut parser = MessageParser::new(network);

    let res = parser.parse_system_transaction(&tx, 0, None);
    if res.is_empty() {
        anyhow::bail!(
            "Inscription not found {:?}",
            propose_sequencer_reveal_tx_id.to_string()
        )
    }

    match res[0] {
        FullInscriptionMessage::ProposeSequencer(_) => true,
        _ => anyhow::bail!("Invalid inscription"),
    };

    let input = ValidatorAttestationInput {
        reference_txid: propose_sequencer_reveal_tx_id,
        attestation: Vote::Ok,
    };
    let validator_info = inscriber
        .inscribe(InscriptionMessage::ValidatorAttestation(input.clone()))
        .await?;
    info!(
        "Validator attestation for propose sequencer tx sent: {:?}",
        &validator_info.final_reveal_tx.txid
    );

    Ok(validator_info.final_reveal_tx.txid)
}

pub fn compute_bridge_address(
    pubkeys: Vec<Musig2PublicKey>,
    network: BitcoinNetwork,
) -> anyhow::Result<BitcoinAddress> {
    let secp = bitcoin::secp256k1::Secp256k1::new();

    let musig_key_agg_cache = KeyAggContext::new(pubkeys)?;

    let agg_pubkey = musig_key_agg_cache.aggregated_pubkey::<secp256k1_musig2::PublicKey>();
    let (xonly_agg_key, _) = agg_pubkey.x_only_public_key();

    // Convert to bitcoin XOnlyPublicKey first
    let internal_key = bitcoin::XOnlyPublicKey::from_slice(&xonly_agg_key.serialize())?;

    // Use internal_key for address creation
    let address = BitcoinAddress::p2tr(&secp, internal_key, None, network);

    Ok(address)
}

fn save_inscription_metadata(data: String, path: String) -> anyhow::Result<()> {
    let mut file = File::create(&path)?;
    file.write_all(data.as_bytes())?;

    println!("JSON {path} file saved successfully!");

    Ok(())
}
