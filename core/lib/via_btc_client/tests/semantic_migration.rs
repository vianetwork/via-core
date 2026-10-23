use std::sync::{Arc, Mutex};

use bitcoin::{blockdata::constants::genesis_block, hashes::Hash, Network};
use via_btc_client::{client::BitcoinClient, migration::ChainArchive, types::NodeAuth};
use via_test_utils::{bitcoin_rpc::BitcoinRpcServer, chain::Chain};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

#[tokio::test]
async fn bootstrap_classification_requires_actual_archived_content_and_correct_network(
) -> anyhow::Result<()> {
    let original = via_test_utils::utils::bootstrap_state_mock();
    let transaction = via_test_utils::utils::bootstrap_transaction(
        &original,
        via_btc_client::types::SYSTEM_BOOTSTRAPPING_MSG.as_bytes(),
    );
    let identity = transaction.compute_txid();
    let mut chain = Chain::default();
    let mut block = genesis_block(Network::Regtest);
    block.txdata = vec![transaction];
    let first = chain.insert(1, block);
    let chain = Arc::new(Mutex::new(chain));
    let served = chain.clone();
    let server = BitcoinRpcServer::new_with_errors(None, false, move |_, request| {
        served.lock().unwrap().rpc(&request)
    })?;
    let client =
        BitcoinClient::new(&server.url(""), NodeAuth::None, ViaBtcClientConfig::for_tests())?;
    let source = ChainArchive::prepare(&client, 0, 1, identity).await?;
    let decoded = source.bootstrap(identity, &client).await?;
    assert_eq!(decoded.wallets, original.wallets);
    assert_eq!(decoded.protocol_version, original.protocol_version);
    assert_eq!(decoded.bootloader_hash, original.bootloader_hash);
    assert_eq!(decoded.base_system_contracts_hashes().evm_emulator, None);
    let mut with_emulator = decoded;
    with_emulator.evm_emulator_hash = zksync_types::H256::repeat_byte(8);
    assert_eq!(
        with_emulator.base_system_contracts_hashes().evm_emulator,
        Some(zksync_types::H256::repeat_byte(8))
    );
    assert!(source.bootstrap(bitcoin::Txid::all_zeros(), &client).await.is_err());
    let mut malformed = source.clone();
    malformed.headers[1].height = 2;
    assert!(malformed.validate().is_err());
    let mut malformed = source.clone();
    malformed.headers[1].header.prev_blockhash = bitcoin::BlockHash::all_zeros();
    assert!(malformed.validate().is_err());
    let mut malformed = source.clone();
    malformed.network = Network::Bitcoin;
    assert!(malformed.validate().is_err());
    chain.lock().unwrap().missing_bodies.insert(first);
    assert!(source.bootstrap(identity, &client).await.is_err());
    chain.lock().unwrap().missing_bodies.clear();
    chain.lock().unwrap().insert(2, genesis_block(Network::Regtest));
    source.revalidate(&client).await?;
    chain.lock().unwrap().fork_at_height(0);
    assert!(source.revalidate(&client).await.is_err());
    let mut foreign = original;
    foreign.wallets.sequencer = bitcoin::Address::from_script(
        &foreign.wallets.sequencer.script_pubkey(),
        Network::Bitcoin,
    )?;
    let transaction = via_test_utils::utils::bootstrap_transaction(
        &foreign,
        via_btc_client::types::SYSTEM_BOOTSTRAPPING_MSG.as_bytes(),
    );
    let identity = transaction.compute_txid();
    let mut block = genesis_block(Network::Regtest);
    block.txdata = vec![transaction];
    chain.lock().unwrap().insert(1, block);
    let foreign = ChainArchive::prepare(&client, 0, 1, identity).await?;
    assert!(foreign.bootstrap(identity, &client).await.is_err());
    Ok(())
}
