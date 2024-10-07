use std::{env, str::FromStr};

use anyhow::{Context, Result};
use bitcoin::{address::NetworkUnchecked, Amount};
use tracing::info;
use via_btc_client::{
    inscriber::Inscriber,
    types::{
        BitcoinAddress, BitcoinNetwork, InscriptionConfig, InscriptionMessage, L1ToL2MessageInput,
        NodeAuth, Recipient,
    },
};
use zksync_types::Address as EVMAddress;

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: BitcoinNetwork = BitcoinNetwork::Regtest;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let depositor_private_key = "cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R".to_string();

    let receiver_l2_address = EVMAddress::from_str("0x36615Cf349d7F6344891B1e7CA7C72883F5dc049")?;

    let bridge_p2wpkh_mpc_address = "bcrt1qdrzjq2mwlhrnhan94em5sl032zd95m73ud8ddw"
        .parse::<BitcoinAddress<NetworkUnchecked>>()?
        .require_network(NETWORK)?;

    let mut inscriber = Inscriber::new(
        RPC_URL,
        NETWORK,
        NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string()),
        &depositor_private_key,
        None,
    )
    .await
    .context("Failed to create Depositor Inscriber")?;

    let args: Vec<String> = env::args().collect();
    let amount = args[1].parse::<f64>()?;
    info!("Depositing {} BTC to L2", amount);

    info!(
        "Depositor L1 balance: {}",
        inscriber
            .get_balance()
            .await
            .context("Failed to get balance")?
    );

    info!("Receiver L2 address: {}", receiver_l2_address);

    let input = L1ToL2MessageInput {
        receiver_l2_address,
        l2_contract_address: EVMAddress::zero(),
        call_data: vec![],
    };

    let deposit_info = inscriber
        .inscribe_with_recipient(
            InscriptionMessage::L1ToL2Message(input),
            InscriptionConfig::default(),
            Some(Recipient {
                address: bridge_p2wpkh_mpc_address,
                amount: Amount::from_btc(amount)?,
            }),
        )
        .await?;

    info!("Deposit tx sent: {:?}", deposit_info.final_reveal_tx.txid);
    info!(
        "Depositor change response: {:?}",
        deposit_info.reveal_tx_output_info.reveal_tx_change_output
    );
    info!(
        "Recipient response: {:?}",
        deposit_info
            .reveal_tx_output_info
            .recipient_tx_output
            .unwrap()
    );

    Ok(())
}
