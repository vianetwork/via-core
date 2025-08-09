use std::{env, str::FromStr, sync::Arc};

use anyhow::{Context, Result};
use bitcoin::{address::NetworkUnchecked, Address};
use tracing::info;
use via_btc_client::{
    client::BitcoinClient,
    indexer::MessageParser,
    inscriber::Inscriber,
    types::{BitcoinNetwork, InscriptionMessage, NodeAuth, UpdateBridgeProposalInput},
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

const TIMEOUT: u64 = 5;

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
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };
    let client = Arc::new(BitcoinClient::new(rpc_url, auth, config)?);
    Inscriber::new(client, signer_private_key, None)
        .await
        .context("Failed to create Inscriber")
}

// Example:
// cargo run --example propose_new_bridge \
//     regtest \
//     http://0.0.0.0:18443 \
//     rpcuser \
//     rpcpassword \
//     cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R \
//     bcrt1p5kp3mnv8tjdly0yyxmed5pl34gy8ufeh9kaf4vk3e7atxrcaq93s7w4xwq \
//     bcrt1q08v0vm5w3rftefqutgtwlyslhy35ms8ftuay80,bcrt1q50xmdcwlmt8qhwczxptaq2h5cn3zchcrvqd35v,bcrt1q9l2wcyaquvvxuzxenae75q24yx4uhzhq3mrlfe

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

    let bridge_address = Address::from_str(&args[6])?.require_network(network)?;
    let verifiers = args[7]
        .clone()
        .split(",")
        .collect::<Vec<&str>>()
        .iter()
        .map(|x| {
            Address::from_str(x)
                .unwrap()
                .require_network(network)
                .unwrap()
        })
        .map(|x| x.as_unchecked().clone())
        .collect::<Vec<Address<NetworkUnchecked>>>();

    info!("Create an propose bridge transaction",);

    let mut inscriber = create_inscriber(
        &private_key,
        &rpc_url,
        &rpc_username,
        &rpc_password,
        network,
    )
    .await?;

    // System contracts Upgrade message
    let input = UpdateBridgeProposalInput {
        bridge_musig2_address: bridge_address.as_unchecked().clone(),
        verifier_p2wpkh_addresses: verifiers,
    };
    let update_bridge_address_proposal = inscriber
        .inscribe(InscriptionMessage::UpdateBridgeProposal(input))
        .await?;
    info!(
        "Update Bridge address proposal: {:?}",
        update_bridge_address_proposal.final_reveal_tx.txid.clone()
    );

    tokio::time::sleep(std::time::Duration::from_secs(TIMEOUT)).await;

    info!("*************************************************************************");

    let mut parser = MessageParser::new(network);
    let tx = inscriber
        .get_client()
        .await
        .get_transaction(&update_bridge_address_proposal.final_reveal_tx.txid)
        .await?;

    let update_bridge_address_proposal = parser.parse_system_transaction(&tx, 0, None);
    info!(
        "Update bridge address proposal: {:?}",
        update_bridge_address_proposal
    );

    Ok(())
}
