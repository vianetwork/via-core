use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};

use bitcoin::{
    address::NetworkUnchecked,
    key::rand,
    script::PushBytesBuf,
    secp256k1::{self, SecretKey},
    CompressedPublicKey, PrivateKey, ScriptBuf, TxOut,
};
use rand::Rng;
use tokio::time::sleep;
use via_btc_client::{
    client::BitcoinClient,
    indexer::{BitcoinInscriptionIndexer, MessageParser},
    inscriber::Inscriber,
    traits::BitcoinOps,
    types::{
        BitcoinAddress,
        BitcoinSecp256k1::hashes::{
            hex::{Case, DisplayHex},
            Hash,
        },
        BitcoinTxid, CommonFields, FullInscriptionMessage, InscriptionMessage,
        L1BatchDAReferenceInput, NodeAuth, ProofDAReferenceInput, UpdateBridge, UpdateBridgeInput,
        UpdateBridgeProposalInput, UpdateGovernance, UpdateGovernanceInput, UpdateSequencer,
        UpdateSequencerInput,
    },
};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;
use zksync_types::{
    via_bootstrap::BootstrapState, via_wallet::SystemWallets, BitcoinNetwork, L1BatchNumber, H256,
};

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

/// Verifier 3 address
pub fn test_verifier_add_3() -> BitcoinAddress {
    BitcoinAddress::from_str(&"bcrt1q23lgaa90s85jvtl6dsrkvn0g949cwjkwuyzwdm")
        .unwrap()
        .assume_checked()
}

pub fn test_wallets() -> SystemWallets {
    let (proposed_sequencer, _) = test_sequencer_wallet();
    SystemWallets {
        sequencer: proposed_sequencer,
        bridge: BitcoinAddress::from_str(
            &"bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq",
        )
        .unwrap()
        .assume_checked(),
        governance: BitcoinAddress::from_str(
            &"bcrt1q92gkfme6k9dkpagrkwt76etkaq29hvf02w5m38f6shs4ddpw7hzqp347zm",
        )
        .unwrap()
        .assume_checked(),
        verifiers: vec![
            test_verifier_add_1(),
            test_verifier_add_2(),
            test_verifier_add_3(),
        ],
    }
}

pub fn bootstrap_state_mock() -> BootstrapState {
    let mut sequencer_votes = HashMap::new();
    sequencer_votes.insert(test_verifier_add_1(), true);
    sequencer_votes.insert(test_verifier_add_2(), true);

    BootstrapState {
        wallets: Some(test_wallets()),
        sequencer_proposal_tx_id: Some(BitcoinTxid::all_zeros()),
        bootstrap_tx_id: Some(BitcoinTxid::all_zeros()),
        sequencer_votes,
        starting_block_number: 1,
        bootloader_hash: Some(H256::zero()),
        abstract_account_hash: Some(H256::zero()),
    }
}

/// Create a update sequencer address inscription
pub fn create_update_sequencer_inscription(address: BitcoinAddress) -> FullInscriptionMessage {
    FullInscriptionMessage::UpdateSequencer(UpdateSequencer {
        common: CommonFields {
            schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64])
                .ok()
                .unwrap(),
            encoded_public_key: PushBytesBuf::new(),
            block_height: 0,
            tx_id: BitcoinTxid::all_zeros(),
            p2wpkh_address: None,
            tx_index: None,
            output_vout: None,
        },
        input: UpdateSequencerInput {
            inputs: vec![],
            address: address.as_unchecked().clone(),
        },
    })
}

/// Create a update governance address inscription
pub fn create_update_governance_inscription(address: BitcoinAddress) -> FullInscriptionMessage {
    FullInscriptionMessage::UpdateGovernance(UpdateGovernance {
        common: CommonFields {
            schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64])
                .ok()
                .unwrap(),
            encoded_public_key: PushBytesBuf::new(),
            block_height: 0,
            tx_id: BitcoinTxid::all_zeros(),
            p2wpkh_address: None,
            tx_index: None,
            output_vout: None,
        },
        input: UpdateGovernanceInput {
            inputs: vec![],
            address: address.as_unchecked().clone(),
        },
    })
}

/// Create a update bridge address inscription
pub async fn create_update_bridge_inscription(
    new_bridge: BitcoinAddress,
    new_verifiers: Vec<BitcoinAddress>,
) -> anyhow::Result<FullInscriptionMessage> {
    let mut inscriber = test_sequencer_inscriber().await?;

    sleep(Duration::from_millis(500)).await;

    let input = UpdateBridgeProposalInput {
        bridge_musig2_address: new_bridge.as_unchecked().clone(),
        verifier_p2wpkh_addresses: new_verifiers
            .iter()
            .map(|address| address.as_unchecked().clone())
            .collect::<Vec<BitcoinAddress<NetworkUnchecked>>>(),
    };

    let result = inscriber
        .inscribe(InscriptionMessage::UpdateBridgeProposal(input))
        .await?;

    Ok(FullInscriptionMessage::UpdateBridge(UpdateBridge {
        common: CommonFields {
            schnorr_signature: bitcoin::taproot::Signature::from_slice(&[0; 64]).unwrap(),
            encoded_public_key: PushBytesBuf::new(),
            block_height: 0,
            tx_id: BitcoinTxid::all_zeros(),
            p2wpkh_address: None,
            tx_index: None,
            output_vout: None,
        },
        input: UpdateBridgeInput {
            inputs: vec![],
            proposal_tx_id: result.final_reveal_tx.txid,
        },
    }))
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
        msgs.extend(parser.parse_system_transaction(&tx, 0, None));
    }

    Ok((msgs, prev_l1_batch_hash))
}

pub fn test_create_indexer() -> BitcoinInscriptionIndexer {
    BitcoinInscriptionIndexer::new(Arc::new(test_bitcoin_client()), Arc::new(test_wallets()))
}

pub fn random_bitcoin_wallet() -> (PrivateKey, BitcoinAddress) {
    // Initialize secp256k1 context
    let secp = secp256k1::Secp256k1::new();

    // Generate a random secret key
    let secret_key = SecretKey::new(&mut rand::thread_rng());

    // Wrap it into a Bitcoin private key
    let private_key = PrivateKey {
        compressed: true,
        network: NETWORK.into(),
        inner: secret_key.clone(),
    };

    let cpk = CompressedPublicKey::from_private_key(&secp, &private_key).unwrap();
    let address = BitcoinAddress::p2wpkh(&cpk, NETWORK);

    (private_key, address)
}

fn dummy_txout(value: u64) -> TxOut {
    TxOut {
        value: bitcoin::Amount::from_sat(value),
        script_pubkey: ScriptBuf::new(),
    }
}

pub fn generate_return_data_per_outputs(count: usize) -> Vec<Vec<u8>> {
    (0..count)
        .map(|_| {
            let mut rng = rand::thread_rng();
            (0..10).map(|_| rng.gen()).collect::<Vec<u8>>()
        })
        .collect()
}

pub fn generate_dummy_outputs(count: usize, value: u64) -> Vec<TxOut> {
    (0..count).map(|_| dummy_txout(value)).collect()
}
