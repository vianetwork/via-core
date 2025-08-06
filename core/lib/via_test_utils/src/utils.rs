use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};

use tokio::time::sleep;
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, BootstrapState, MessageParser},
    inscriber::Inscriber,
    traits::BitcoinOps,
    types::{
        BitcoinAddress,
        BitcoinSecp256k1::hashes::{
            hex::{Case, DisplayHex},
            Hash,
        },
        BitcoinTxid, FullInscriptionMessage, InscriptionMessage, L1BatchDAReferenceInput, NodeAuth,
        ProofDAReferenceInput, Vote,
    },
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::{BitcoinNetwork, L1BatchNumber, H256};

const RPC_URL: &str = "http://0.0.0.0:18443";
const RPC_USERNAME: &str = "rpcuser";
const RPC_PASSWORD: &str = "rpcpassword";
const NETWORK: BitcoinNetwork = BitcoinNetwork::Regtest;

/// Init a BTC client
pub fn test_bitcoin_client() -> BitcoinClient {
    BitcoinClient::new(
        RPC_URL,
        NodeAuth::UserPass(RPC_USERNAME.into(), RPC_PASSWORD.into()),
        ViaBtcClientConfig {
            network: NETWORK.to_string(),
            external_apis: vec!["https://mempool.space/testnet/api/v1/fees/recommended".into()],
            fee_strategies: vec!["fastestFee".into()],
            use_rpc_for_fee_rate: None,
        },
    )
    .unwrap()
}

/// Return the sequencer address and PK
pub fn test_sequencer_wallet() -> (BitcoinAddress, String) {
    (
        BitcoinAddress::from_str(&"bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56")
            .unwrap()
            .assume_checked(),
        "cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R".into(),
    )
}

/// Init a BTC inscriber
pub async fn test_sequencer_inscriber() -> anyhow::Result<Inscriber> {
    let (_, signer_private_key) = test_sequencer_wallet();
    Inscriber::new(Arc::new(test_bitcoin_client()), &signer_private_key, None).await
}

/// Verifier 1 address
pub fn test_verifier_add_1() -> BitcoinAddress {
    BitcoinAddress::from_str(&"bcrt1qw2mvkvm6alfhe86yf328kgvr7mupdx4vln7kpv")
        .unwrap()
        .assume_checked()
}

/// Verifier 2 address
pub fn test_verifier_add_2() -> BitcoinAddress {
    BitcoinAddress::from_str(&"bcrt1qk8mkhrmgtq24nylzyzejznfzws6d98g4kmuuh4")
        .unwrap()
        .assume_checked()
}

pub fn bootstrap_state_mock() -> BootstrapState {
    let mut sequencer_votes = HashMap::new();
    sequencer_votes.insert(test_verifier_add_1(), Vote::Ok);
    sequencer_votes.insert(test_verifier_add_2(), Vote::Ok);

    let (proposed_sequencer, _) = test_sequencer_wallet();
    BootstrapState {
        verifier_addresses: vec![test_verifier_add_1(), test_verifier_add_2()],
        proposed_sequencer: Some(proposed_sequencer),
        proposed_sequencer_txid: Some(BitcoinTxid::all_zeros()),
        sequencer_votes,
        bridge_address: Some(
            BitcoinAddress::from_str(
                &"bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq",
            )
            .unwrap()
            .assume_checked(),
        ),
        starting_block_number: 1,
        bootloader_hash: Some(H256::zero()),
        abstract_account_hash: Some(H256::zero()),
        proposed_governance: Some(
            BitcoinAddress::from_str(
                &"bcrt1q92gkfme6k9dkpagrkwt76etkaq29hvf02w5m38f6shs4ddpw7hzqp347zm",
            )
            .unwrap()
            .assume_checked(),
        ),
    }
}

/// Returns the inscriptions and the last block hash
pub async fn create_chained_inscriptions(
    start: usize,
    end: usize,
    prev_batch_hash_opt: Option<H256>,
) -> anyhow::Result<(Vec<FullInscriptionMessage>, H256)> {
    let mut inscriber = test_sequencer_inscriber().await?;
    let client = test_bitcoin_client();
    let mut parser = MessageParser::new(NETWORK);

    let mut msgs = vec![];
    let mut prev_l1_batch_hash = H256::zero();
    if let Some(prev_batch_hash) = prev_batch_hash_opt {
        prev_l1_batch_hash = prev_batch_hash;
    }

    for i in start..(end + 1) {
        let l1_batch_hash = H256::random();
        sleep(Duration::from_millis(500)).await;

        let batch_pubdata = L1BatchDAReferenceInput {
            l1_batch_hash: l1_batch_hash.clone(),
            blob_id: H256::random().0.to_hex_string(Case::Lower),
            da_identifier: "celestia".into(),
            l1_batch_index: L1BatchNumber(i as u32),
            prev_l1_batch_hash: prev_l1_batch_hash.clone(),
        };

        prev_l1_batch_hash = l1_batch_hash;

        let result = inscriber
            .inscribe(InscriptionMessage::L1BatchDAReference(
                batch_pubdata.clone(),
            ))
            .await?;

        sleep(Duration::from_millis(500)).await;

        let batch_proof = ProofDAReferenceInput {
            l1_batch_reveal_txid: result.final_reveal_tx.txid,
            da_identifier: "celestia".into(),
            blob_id: H256::random().0.to_hex_string(Case::Lower),
        };

        let result = inscriber
            .inscribe(InscriptionMessage::ProofDAReference(batch_proof.clone()))
            .await?;
        sleep(Duration::from_millis(500)).await;

        let tx = client.get_transaction(&result.final_reveal_tx.txid).await?;
        msgs.extend(parser.parse_system_transaction(&tx, 0));
    }

    Ok((msgs, prev_l1_batch_hash))
}

pub async fn test_create_indexer() -> anyhow::Result<BitcoinInscriptionIndexer> {
    let bootstrap_state = bootstrap_state_mock();

    let client = test_bitcoin_client();
    let parser = MessageParser::new(BitcoinNetwork::Regtest);

    Ok(BitcoinInscriptionIndexer::create_indexer(
        bootstrap_state,
        Arc::new(client.clone()),
        parser.clone(),
    )?)
}
