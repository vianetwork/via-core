use std::sync::Arc;

use anyhow::{Context, Result};
use via_btc_client::{
    client::BitcoinClient,
    inscriber::Inscriber,
    types::{self as inscribe_types, BitcoinNetwork, NodeAuth},
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: BitcoinNetwork = BitcoinNetwork::Regtest;

#[tokio::main]
async fn main() -> Result<()> {
    // get the node url and private key from the environment

    // export BITCOIN_NODE_URL="http://example.com:8332"
    // export BITCOIN_PRV=example_wif

    let rpc_url = std::env::var("BITCOIN_NODE_URL").context("BITCOIN_NODE_URL not set")?;
    let prv = std::env::var("BITCOIN_PRV").context("BITCOIN_PRV not set")?;
    let auth = NodeAuth::UserPass(RPC_USERNAME.to_string(), RPC_PASSWORD.to_string());
    let config = ViaBtcClientConfig {
        network: NETWORK.to_string(),
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };
    let client = Arc::new(BitcoinClient::new(&rpc_url, auth, config)?);

    let mut inscriber_instance = Inscriber::new(client, &prv, None)
        .await
        .context("Failed to create Inscriber")?;

    println!(
        "balance: {}",
        inscriber_instance
            .get_balance()
            .await
            .context("Failed to get balance")?
    );

    let l1_da_batch_ref = inscribe_types::L1BatchDAReferenceInput {
        l1_batch_hash: zksync_basic_types::H256([0; 32]),
        l1_batch_index: zksync_basic_types::L1BatchNumber(0_u32),
        da_identifier: "da_identifier_celestia".to_string(),
        blob_id: "batch_temp_blob_id".to_string(),
        prev_l1_batch_hash: zksync_basic_types::H256([0; 32]),
    };

    let inscribe_info = inscriber_instance
        .inscribe(inscribe_types::InscriptionMessage::L1BatchDAReference(
            l1_da_batch_ref,
        ))
        .await
        .context("Failed to inscribe L1BatchDAReference")?;

    println!("---------------------------------First Inscription---------------------------------");
    let context = inscriber_instance.get_context_snapshot()?;
    println!("context: {:?}", context);

    let l1_da_proof_ref = inscribe_types::ProofDAReferenceInput {
        l1_batch_reveal_txid: inscribe_info.final_reveal_tx.txid,
        da_identifier: "da_identifier_celestia".to_string(),
        blob_id: "proof_temp_blob_id".to_string(),
    };

    let _da_proof_ref_reveal_txid = inscriber_instance
        .inscribe(inscribe_types::InscriptionMessage::ProofDAReference(
            l1_da_proof_ref,
        ))
        .await
        .context("Failed to inscribe ProofDAReference")?;

    println!(
        "---------------------------------Second Inscription---------------------------------"
    );
    let context = inscriber_instance.get_context_snapshot()?;

    println!("context: {:?}", context);

    println!("---------------------------------End---------------------------------");

    Ok(())
}
