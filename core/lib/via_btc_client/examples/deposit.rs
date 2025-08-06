use std::{
    env,
    fs::File,
    io::{Read, Write},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result};
use bitcoin::Amount;
use tracing::info;
use via_btc_client::{
    client::BitcoinClient,
    inscriber::Inscriber,
    types::{
        BitcoinAddress, BitcoinNetwork, InscriberContext, InscriptionMessage, L1ToL2MessageInput,
        NodeAuth, Recipient,
    },
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::Address as EVMAddress;

const CONTEXT_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/depositor_inscriber_context.json"
);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args: Vec<String> = env::args().collect();
    let amount = args[1].parse::<u64>()?;

    let receiver_l2_address = EVMAddress::from_str(&args[2])?;
    info!(
        "Depositing {} BTC to receiver L2 address {}",
        amount, receiver_l2_address
    );

    let depositor_private_key = args[3].clone();
    info!(
        "Depositor L1 private key: {}...{}",
        &depositor_private_key[..4],
        &depositor_private_key[depositor_private_key.len() - 4..]
    );

    let network: BitcoinNetwork = args[4].parse().expect("Invalid network value");
    let rpc_url = args[5].clone();
    let rpc_username = args[6].clone();
    let rpc_password = args[7].clone();
    let bridge_musig2_address_str = args[8].clone();

    let bridge_musig2_address =
        BitcoinAddress::from_str(&bridge_musig2_address_str)?.require_network(network)?;

    // Load the previous context from the file if it exists
    let context = load_context_from_file(CONTEXT_FILE)?;

    let auth = NodeAuth::UserPass(rpc_username.to_string(), rpc_password.to_string());
    let config = ViaBtcClientConfig {
        network: network.to_string(),
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };
    let client = Arc::new(BitcoinClient::new(&rpc_url, auth, config)?);

    let mut inscriber = Inscriber::new(client, &depositor_private_key, context)
        .await
        .context("Failed to create Depositor Inscriber")?;

    info!(
        "Depositor L1 balance: {}",
        inscriber
            .get_balance()
            .await
            .context("Failed to get balance")?
    );

    let input = L1ToL2MessageInput {
        receiver_l2_address,
        l2_contract_address: EVMAddress::zero(),
        call_data: vec![],
    };

    let deposit_info = inscriber
        .inscribe_with_recipient(
            InscriptionMessage::L1ToL2Message(input),
            Some(Recipient {
                address: bridge_musig2_address,
                amount: Amount::from_sat(amount),
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

    // Save the updated context to the file after the inscription
    save_context_to_file(&inscriber.get_context_snapshot()?, CONTEXT_FILE)?;

    Ok(())
}

// Utility function to save the context to a file
fn save_context_to_file(context: &InscriberContext, file_path: &str) -> Result<()> {
    let serialized_context = serde_json::to_string(context)?;
    let mut file = File::create(file_path)?;
    file.write_all(serialized_context.as_bytes())?;
    Ok(())
}

// Utility function to load the context from a file
fn load_context_from_file(file_path: &str) -> Result<Option<InscriberContext>> {
    if let Ok(mut file) = File::open(file_path) {
        let mut data = String::new();
        file.read_to_string(&mut data)?;
        let context: InscriberContext = serde_json::from_str(&data)?;
        Ok(Some(context))
    } else {
        Ok(None)
    }
}
