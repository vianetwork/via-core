use std::{env, str::FromStr};

use anyhow::{Context, Result};
use tracing::info;
use via_btc_client::{
    indexer::MessageParser,
    inscriber::Inscriber,
    types::{BitcoinNetwork, InscriptionMessage, NodeAuth, SystemContractUpgradeInput},
};
use zksync_basic_types::H256;
use zksync_types::{protocol_version::ProtocolSemanticVersion, H160};

const TIMEOUT: u64 = 5;

async fn create_inscriber(
    signer_private_key: &str,
    rpc_url: &str,
    rpc_username: &str,
    rpc_password: &str,
    network: BitcoinNetwork,
) -> Result<Inscriber> {
    Inscriber::new(
        rpc_url,
        network,
        NodeAuth::UserPass(rpc_username.to_string(), rpc_password.to_string()),
        signer_private_key,
        None,
    )
    .await
    .context("Failed to create Inscriber")
}

// Example:
// cargo run --example upgrade_system_contracts \
//     regtest \
//     http://0.0.0.0:18443 \
//     rpcuser \
//     rpcpassword \
//     cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R \
//     0.26.0 \
//     0x010008e79c154523aa30981e598b73c4a33c304bef9c82bae7d2ca4d21daedc7 \
//     0x010005630848b5537f934eea6bd8c61c50648a162b90c82f316454f4109462b1 \
//     0x0000000000000000000000000000000000008002,0x0000000000000000000000000000000000008003,0x0000000000000000000000000000000000008004,0x0000000000000000000000000000000000008005,0x0000000000000000000000000000000000008006,0x0000000000000000000000000000000000008008,0x0000000000000000000000000000000000008009,0x000000000000000000000000000000000000800a,0x000000000000000000000000000000000000800b,0x000000000000000000000000000000000000800c,0x000000000000000000000000000000000000800e,0x000000000000000000000000000000000000800f,0x0000000000000000000000000000000000008011,0x0000000000000000000000000000000000010000 \
//     0x010000758176955a4cb7af4c768cdad127fa6b1d0329ce3e8b37d1382e9c21e6,0x010000e519c333515fb2902b43b73753e471b5f050afb7253d2b7ca1a0ca28dd,0x0100007de7fbc3f23bf798821d617e7abf35543d67d691daf4ab33d6aa0aaaa1,0x0100003d2b00580834cc0ba74783182bcd1c9be63b81326e6d9d1242f2e72d63,0x010005213e9b151eab1dd03eba19219c7c4c8778ecdd9d3520665f49794cd56e,0x010002b9105e8d0d43d758b2e884bbe97b301cab1780ecc2d9f8206ed4adcb1b,0x01000069064aa420a4e8004272ef59378f26c59ae84f52fbe5df7d516942f136,0x010000d992045634736e90b5c8372cbb9520c86601409d6b5117817034ba539f,0x010001b37de496022dfcb13fd8e2921803e4d71e92b54c88a97f6069606829ee,0x010007d183ba925a6de81217c6b7e19466f07e40a90f970a9d0a63355e30fde0,0x01000179c8d70de580c7622ce075ac6a77ba56045db7d8fe543b0c013b468182,0x01000055ed1cea72303c74522c33318548c6dc6c8c38c03a0071f6a55ec97a0a,0x010000496f7a43dbb6796f6e89e322895f8a50de5b26d3982f0e5484f5c454e1,0x0100004b55461205f9eb98f3c07a0f6dbf4e16cbddb015a68816c9f20930d46a \
//     0x14f97b81e54b35fe673d8708cc1a19e1ea5b5e348e12d31e39824ed4f42bbca2

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args: Vec<String> = env::args().collect();
    let network = BitcoinNetwork::from_str(&args[1].clone())?;
    let rpc_url = args[2].clone();
    let rpc_username = args[3].clone();
    let rpc_password = args[4].clone();
    let private_key = args[5].clone();

    let version = ProtocolSemanticVersion::from_str(&args[6])?;
    let bootloader_code_hash = H256::from_str(&args[7])?;
    let default_account_code_hash = H256::from_str(&args[8])?;
    let system_contracts_addresses = args[9]
        .clone()
        .split(",")
        .collect::<Vec<&str>>()
        .iter()
        .map(|x| H160::from_str(x).unwrap())
        .collect::<Vec<H160>>();
    let system_contracts_hashes = args[10]
        .clone()
        .split(",")
        .collect::<Vec<&str>>()
        .iter()
        .map(|x| H256::from_str(x).unwrap())
        .collect::<Vec<H256>>();

    let system_contracts: Vec<(H160, H256)> = system_contracts_addresses
        .iter()
        .cloned()
        .zip(system_contracts_hashes.iter().cloned())
        .collect();
    let recursion_scheduler_level_vk_hash = H256::from_str(&args[11])?;

    info!(
        "Create an upgrade transaction for protocol version {}",
        version
    );

    let mut inscriber = create_inscriber(
        &private_key,
        &rpc_url,
        &rpc_username,
        &rpc_password,
        network,
    )
    .await?;

    // System contracts Upgrade message
    let input = SystemContractUpgradeInput {
        version,
        bootloader_code_hash,
        default_account_code_hash,
        recursion_scheduler_level_vk_hash,
        system_contracts,
    };
    let system_contract_upgrade_info = inscriber
        .inscribe(InscriptionMessage::SystemContractUpgrade(input))
        .await?;
    info!(
        "System contract upgrade info tx sent: {:?}",
        system_contract_upgrade_info.final_reveal_tx.txid.clone()
    );

    tokio::time::sleep(std::time::Duration::from_secs(TIMEOUT)).await;

    info!("*************************************************************************");

    let mut parser = MessageParser::new(network);
    let tx = inscriber
        .get_client()
        .await
        .get_transaction(&system_contract_upgrade_info.final_reveal_tx.txid)
        .await?;

    let upgrade_inscription = parser.parse_system_transaction(&tx, 0);
    info!("upgrade_inscription: {:?}", upgrade_inscription);

    Ok(())
}
