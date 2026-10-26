use std::sync::Arc;

use bitcoin::{hashes::Hash, BlockHash, Network, Txid};
use transition::{PreparedTransition, ReaderSnapshot, RetainedProofs, Transition};
use via_btc_client::{
    client::BitcoinClient,
    indexer::{
        configuration::ConfigurationInput,
        replay::{Included, MessageOrigin, Payload, ReplayConflict},
    },
    types::UpdateGovernanceInput,
};
use via_btc_storage::*;
use via_reorg::chain::{BlockRef, Header};
use via_test_utils::{bitcoin_core::BitcoinCore, chain::Chain};
use zksync_dal::Core as Test;
use zksync_db_connection::{
    connection::Connection, connection_pool::ConnectionPool, instrument::InstrumentExt,
};
use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};

fn fixture_header(chain: &Chain, height: u32) -> Header<BlockHash> {
    Header {
        height,
        block_hash: chain.blocks[&height].block_hash(),
        parent_hash: chain.blocks[&height].header.prev_blockhash,
    }
}
async fn initialized_pool(
    start: Header<BlockHash>,
    selected: BlockRef<BlockHash>,
) -> anyhow::Result<ConnectionPool<Test>> {
    let pool = ConnectionPool::<Test>::test_pool().await;
    let mut db = pool.connection().await?;
    let mut tx = db.start_transaction().await?;
    ReaderDal::new(&mut tx).initialize("regtest", start.block_hash, start, selected).await?;
    tx.commit().await?;
    Ok(pool)
}
async fn synthetic_pool() -> anyhow::Result<(Chain, ConnectionPool<Test>)> {
    let chain = Chain::with_height(2);
    let pool =
        initialized_pool(fixture_header(&chain, 0), fixture_header(&chain, 2).into()).await?;
    Ok((chain, pool))
}
async fn append_pair(
    db: &mut Connection<'_, Test>,
    chain: &Chain,
) -> anyhow::Result<[StoredHeader; 3]> {
    for height in 1..=2 {
        let mut reader = ReaderDal::new(db);
        let previous = reader.get_highest_block().await?.unwrap();
        reader.save_block(fixture_header(chain, height), &previous).await?;
    }
    let mut reader = ReaderDal::new(db);
    Ok([
        reader.header_at(0).await?.unwrap(),
        reader.header_at(1).await?.unwrap(),
        reader.header_at(2).await?.unwrap(),
    ])
}
async fn appended_pair(
) -> anyhow::Result<(ConnectionPool<Test>, Connection<'static, Test>, [StoredHeader; 3])> {
    let (chain, pool) = synthetic_pool().await?;
    let mut tx = pool.start_transaction().await?;
    let headers = append_pair(&mut tx, &chain).await?;
    Ok((pool, tx, headers))
}
async fn child(db: &mut Connection<'_, Test>, header: Header<BlockHash>) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO via_btc_configuration(l1_block_number,block_hash,transaction_index,message_index,txid,role,payload) VALUES($1,$2,0,0,'child','Gov','{}')")
        .bind(i64::from(header.height)).bind(header.block_hash.to_string()).instrument("reader_test_child").execute(db).await?;
    Ok(())
}
async fn children(db: &mut Connection<'_, Test>) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM via_btc_configuration")
        .instrument("reader_test_children")
        .fetch_one(db)
        .await?)
}
#[tokio::test]
async fn journal_effects_are_atomic_across_append_and_rewind() -> anyhow::Result<()> {
    let (chain, pool) = synthetic_pool().await?;
    let mut db = pool.connection().await?;
    let anchor = ReaderDal::new(&mut db).header_at(0).await?.unwrap();
    for rewind in [false, true] {
        for commit in [false, true] {
            let mut tx = db.start_transaction().await?;
            if rewind {
                let first = ReaderDal::new(&mut tx).header_at(1).await?.unwrap();
                ReaderDal::new(&mut tx).delete_common_suffix(0).await?;
                ReaderDal::new(&mut tx)
                    .rewind_headers(&first, &anchor, anchor.header.into())
                    .await?;
            } else {
                child(&mut tx, fixture_header(&chain, 1)).await?;
                ReaderDal::new(&mut tx).save_block(fixture_header(&chain, 1), &anchor).await?;
            }
            if commit {
                tx.commit().await?;
            } else {
                drop(tx);
            }
            let expected = u32::from(commit != rewind);
            let mut fresh = pool.connection().await?;
            assert_eq!(
                ReaderDal::new(&mut fresh).get_highest_block().await?.unwrap().header.height,
                expected
            );
            assert_eq!(children(&mut fresh).await?, i64::from(expected));
        }
    }
    Ok(())
}
#[tokio::test]
async fn consumer_progress_requires_next_or_same_header() -> anyhow::Result<()> {
    let (pool, mut tx, [root, first, second]) = appended_pair().await?;
    let mut reader = ReaderDal::new(&mut tx);
    reader.complete_consumer("ordered", &root, &root).await?;
    assert!(reader.complete_consumer("ordered", &second, &root).await.is_err());
    reader.complete_consumer("ordered", &first, &root).await?;
    reader.complete_consumer("ordered", &second, &root).await?;
    reader.complete_consumer("ordered", &second, &first).await?;
    tx.commit().await?;
    let mut db = pool.connection().await?;
    let position = ReaderDal::new(&mut db).consumer("ordered").await?.unwrap();
    assert_eq!(position.applied, second);
    assert_eq!(position.oldest_needed, first);
    Ok(())
}
#[tokio::test]
async fn rewind_preserves_frontier_and_new_incarnation() -> anyhow::Result<()> {
    let (_pool, mut tx, [root, first, second]) = appended_pair().await?;
    let mut reader = ReaderDal::new(&mut tx);
    reader.advance_frontier(&second, &first).await?;
    let error = reader.rewind_headers(&second, &root, root.header.into()).await.unwrap_err();
    assert!(matches!(error.downcast_ref::<StoreError>(), Some(StoreError::Integrity(_))));
    reader.rewind_headers(&second, &first, first.header.into()).await?;
    assert!(reader.save_block(second.header, &first).await? > second.id);
    assert_eq!(reader.state().await?.unwrap().confirmed, first.header.into());
    tx.commit().await?;
    Ok(())
}
#[tokio::test]
async fn replay_waits_for_configuration_decisions_and_required_canonical_positions(
) -> anyhow::Result<()> {
    let (chain, pool) = synthetic_pool().await?;
    let mut db = pool.connection().await?;
    let mut tx = db.start_transaction().await?;
    append_pair(&mut tx, &chain).await?;
    assert!(!ReaderDal::new(&mut tx).complete_replay().await?);
    let root = ReaderDal::new(&mut tx).header_at(0).await?.unwrap();
    for height in 0..=2 {
        let block = ReaderDal::new(&mut tx).header_at(height).await?.unwrap();
        ReaderDal::new(&mut tx).complete_consumer(PROTOCOL_CONSUMER, &block, &root).await?;
    }
    for query in ["UPDATE via_btc_reader SET replay_cutoff_hash=(SELECT hash FROM via_btc_headers WHERE number=0)","UPDATE via_btc_reader SET replay_cutoff_hash=(SELECT hash FROM via_btc_headers WHERE number=2),confirmed_hash=(SELECT hash FROM via_btc_headers WHERE number=2)"] {
        sqlx::query(query).instrument("reader_test_incomplete_source").execute(&mut tx).await?;
        assert!(!ReaderDal::new(&mut tx).complete_replay().await?);
    }
    sqlx::query("UPDATE via_btc_reader SET confirmed_hash=(SELECT hash FROM via_btc_headers WHERE number=0)").instrument("reader_test_restore_source").execute(&mut tx).await?;
    child(&mut tx, fixture_header(&chain, 1)).await?;
    assert!(!ReaderDal::new(&mut tx).complete_replay().await?);
    sqlx::query("UPDATE via_btc_configuration SET accepted=FALSE")
        .instrument("reader_test_decide")
        .execute(&mut tx)
        .await?;
    assert!(ReaderDal::new(&mut tx).complete_replay().await?);
    tx.commit().await?;
    Ok(())
}

async fn configuration_fixture() -> anyhow::Result<(
    ConnectionPool<Test>,
    Connection<'static, Test>,
    Included<ConfigurationInput>,
    SystemWallets,
)> {
    let (chain, pool) = synthetic_pool().await?;
    let wallets = via_test_utils::utils::test_wallets();
    let mut tx = pool.start_transaction().await?;
    append_pair(&mut tx, &chain).await?;
    let mut reader = ReaderDal::new(&mut tx);
    reader
        .insert_wallets(&SystemWalletsDetails::try_from(wallets.clone())?, WalletSource::Unbound(0))
        .await?;
    let root = reader.header_at(0).await?.unwrap();
    for height in 0..=2 {
        let block = reader.header_at(height).await?.unwrap();
        reader.complete_consumer(PROTOCOL_CONSUMER, &block, &root).await?;
    }
    let change = Included {
        origin: MessageOrigin {
            height: 1,
            hash: fixture_header(&chain, 1).block_hash,
            transaction_index: 0,
            message_index: 0,
            txid: Txid::from_byte_array([1; 32]),
        },
        payload: Payload {
            input: ConfigurationInput::Governance(UpdateGovernanceInput {
                inputs: vec![],
                address: wallets.sequencer.as_unchecked().clone(),
            }),
            public_key: vec![],
            signature: vec![0; 64],
            sender_script: None,
        },
    };
    Ok((pool, tx, change, wallets))
}
#[tokio::test]
async fn configuration_occurrence_retry_compares_payload() -> anyhow::Result<()> {
    let (_pool, mut tx, mut change, _) = configuration_fixture().await?;
    let mut reader = ReaderDal::new(&mut tx);
    let id = reader.save_configuration(&change).await?;
    assert_eq!(reader.save_configuration(&change).await?, id);
    change.payload.signature[0] = 1;
    assert_eq!(
        reader
            .save_configuration(&change)
            .await
            .unwrap_err()
            .downcast_ref::<ReplayConflict>()
            .unwrap()
            .field,
        "signature"
    );
    Ok(())
}
#[tokio::test]
async fn same_block_role_conflicts_are_rejected() -> anyhow::Result<()> {
    let (_pool, mut tx, mut change, wallets) = configuration_fixture().await?;
    let mut reader = ReaderDal::new(&mut tx);
    let first = reader.save_configuration(&change).await?;
    change.origin.message_index = 1;
    assert_ne!(reader.save_configuration(&change).await?, first);
    reader.decide_configurations(1, &[], &[], true, Network::Regtest, 0.66).await?;
    assert!(reader.configurations_at(1).await?.iter().all(|c| c.accepted == Some(false)));
    assert_eq!(reader.wallets_before(2).await?, Some(wallets));
    Ok(())
}
#[tokio::test]
async fn configuration_activates_next_height_and_unwinds() -> anyhow::Result<()> {
    let (_pool, mut tx, mut change, wallets) = configuration_fixture().await?;
    let mut reader = ReaderDal::new(&mut tx);
    let first = reader.header_at(1).await?.unwrap();
    let second = reader.header_at(2).await?.unwrap();
    change.origin.height = 2;
    change.origin.hash = second.header.block_hash;
    reader.save_configuration(&change).await?;
    assert!(reader.wallets_before(3).await?.is_none());
    assert_eq!(reader.wallets_before(2).await?, Some(wallets.clone()));
    reader.decide_configurations(2, &[], &[], true, Network::Regtest, 0.66).await?;
    let mut expected = wallets.clone();
    expected.governance = wallets.sequencer.clone();
    assert_eq!(reader.wallets_before(3).await?, Some(expected));
    reader.delete_common_suffix(1).await?;
    reader.rewind_headers(&second, &first, first.header.into()).await?;
    assert_eq!(reader.wallets_before(3).await?, Some(wallets));
    Ok(())
}

async fn core_fixture() -> anyhow::Result<(BitcoinCore, Arc<BitcoinClient>, ConnectionPool<Test>)> {
    let core = BitcoinCore::new()?;
    let client = core.client()?;
    let selected = core.mine(1)?[0];
    let genesis = client.canonical_header_at(0).await?;
    let pool = initialized_pool(
        Header { height: 0, block_hash: genesis.hash, parent_hash: genesis.parent_hash },
        BlockRef { height: 1, block_hash: selected },
    )
    .await?;
    Ok((core, client, pool))
}

#[tokio::test]
async fn header_lookup_retries_branch_switch_but_stops_inconsistent_height() -> anyhow::Result<()> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };

    use via_test_utils::bitcoin_rpc::BitcoinRpcServer;
    let chain = Chain::with_height(2);
    let pool =
        initialized_pool(fixture_header(&chain, 0), fixture_header(&chain, 2).into()).await?;
    let chain = Arc::new(Mutex::new(chain));
    let switched = Arc::new(AtomicBool::new(false));
    let inconsistent_height = Arc::new(AtomicBool::new(false));
    let server = BitcoinRpcServer::new_with_errors(None, false, {
        let chain = chain.clone();
        let switched = switched.clone();
        let inconsistent_height = inconsistent_height.clone();
        move |_, request| {
            let mut chain = chain.lock().unwrap();
            let result = if inconsistent_height.load(Ordering::SeqCst)
                && request["method"] == "getblockhash"
                && request["params"][0] == 2
            {
                Ok(chain.blocks[&1].block_hash().to_string().into())
            } else {
                chain.rpc(&request)
            };
            if request["method"] == "getblockhash"
                && request["params"][0] == 1
                && !switched.swap(true, Ordering::SeqCst)
            {
                chain.fork_at_height(0);
            }
            result
        }
    })?;
    let client = Arc::new(BitcoinClient::new(
        &server.url(""),
        via_btc_client::types::NodeAuth::None,
        zksync_config::configs::via_btc_client::ViaBtcClientConfig::for_tests(),
    )?);
    let mut indexer = via_btc_client::indexer::BitcoinInscriptionIndexer::new(
        client.clone(),
        Arc::new(via_test_utils::utils::test_wallets()),
    );
    let mut db = pool.connection().await?;
    let mut tx = db.start_transaction().await?;
    let mut reader = ReaderDal::new(&mut tx);
    let root = reader.header_at(0).await?.unwrap();
    reader.complete_consumer(PROTOCOL_CONSUMER, &root, &root).await?;
    tx.commit().await?;
    let error = transition::process_next(&mut db, &client, &mut indexer, 0.3).await.unwrap_err();
    assert!(transition::is_held(&error), "{error:#}");
    let state = ReaderDal::new(&mut db).state().await?.unwrap();
    assert!(state.stopped_reason.is_none());
    assert_eq!(ReaderDal::new(&mut db).get_highest_block().await?.unwrap().header.height, 0);
    assert!(transition::process_next(&mut db, &client, &mut indexer, 0.3).await?);
    assert_eq!(
        ReaderDal::new(&mut db).get_highest_block().await?.unwrap().header,
        fixture_header(&chain.lock().unwrap(), 1)
    );
    inconsistent_height.store(true, Ordering::SeqCst);
    let error = transition::process_next(&mut db, &client, &mut indexer, 0.3).await.unwrap_err();
    assert!(
        matches!(error.downcast_ref::<StoreError>(), Some(StoreError::Stopped(_))),
        "{error:#}"
    );
    assert!(!transition::is_held(&error));
    let state = ReaderDal::new(&mut db).state().await?.unwrap();
    assert!(state.stopped_reason.is_some());
    assert!(!state.replay_ready);
    assert_eq!(ReaderDal::new(&mut db).get_highest_block().await?.unwrap().header.height, 1);
    inconsistent_height.store(false, Ordering::SeqCst);
    drop(db);
    let mut reopened = pool.connection().await?;
    let error =
        transition::process_next(&mut reopened, &client, &mut indexer, 0.3).await.unwrap_err();
    assert!(matches!(error.downcast_ref::<StoreError>(), Some(StoreError::Stopped(_))));
    let reopened_state = ReaderDal::new(&mut reopened).state().await?.unwrap();
    assert_eq!(reopened_state.stopped_reason, state.stopped_reason);
    assert!(!reopened_state.replay_ready);
    Ok(())
}

#[tokio::test]
async fn authority_validates_headers_once_without_downloading_unused_body() -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let chain = Chain::with_height(1);
    let root = fixture_header(&chain, 0);
    let pool = initialized_pool(root, root.into()).await?;
    let mut tx = pool.start_transaction().await?;
    let mut reader = ReaderDal::new(&mut tx);
    let stored = reader.header_at(0).await?.unwrap();
    reader.complete_consumer(PROTOCOL_CONSUMER, &stored, &stored).await?;
    assert!(reader.complete_replay().await?);
    tx.commit().await?;
    let bodies = Arc::new(AtomicUsize::new(0));
    let tips = Arc::new(AtomicUsize::new(0));
    let server = via_test_utils::bitcoin_rpc::BitcoinRpcServer::new_with_errors(None, false, {
        let bodies = bodies.clone();
        let tips = tips.clone();
        let confirmations = AtomicUsize::new(1);
        move |_, request| {
            let mut result = chain.rpc(&request)?;
            match request["method"].as_str() {
                Some("getblock") => {
                    bodies.fetch_add(1, Ordering::SeqCst);
                }
                Some("getbestblockhash") => {
                    tips.fetch_add(1, Ordering::SeqCst);
                }
                Some("getblockheader") => {
                    result["confirmations"] = confirmations.fetch_add(1, Ordering::SeqCst).into();
                }
                _ => {}
            }
            Ok(result)
        }
    })?;
    let client = BitcoinClient::new(
        &server.url(""),
        via_btc_client::types::NodeAuth::None,
        zksync_config::configs::via_btc_client::ViaBtcClientConfig::for_tests(),
    )?;
    let error = transition::begin_authority(&pool, &client, &[]).await.err().unwrap();
    assert!(
        matches!(error.downcast_ref::<StoreError>(), Some(StoreError::AuthorityHeld)),
        "{error:#}"
    );
    assert_eq!(bodies.load(Ordering::SeqCst), 0);
    assert!(tips.load(Ordering::SeqCst) <= 2);
    Ok(())
}
async fn prepare(
    client: &BitcoinClient,
    db: &mut Connection<'_, Test>,
) -> anyhow::Result<PreparedTransition> {
    let snapshot =
        ReaderSnapshot::load(&mut ReaderDal::new(db), client.reorg_safe_depth()?).await?;
    PreparedTransition::prepare_headers(client, snapshot).await
}
async fn apply_first_and_complete(
    client: &BitcoinClient,
    db: &mut Connection<'_, Test>,
) -> anyhow::Result<()> {
    let prepared = prepare(client, db).await?;
    let mut tx = db.start_transaction().await?;
    prepared.apply_headers(&mut ReaderDal::new(&mut tx)).await?;
    let mut reader = ReaderDal::new(&mut tx);
    let oldest = reader.header_at(0).await?.unwrap();
    let first = reader.header_at(1).await?.unwrap();
    reader.complete_consumer(PROTOCOL_CONSUMER, &oldest, &oldest).await?;
    reader.complete_consumer(PROTOCOL_CONSUMER, &first, &oldest).await?;
    assert!(reader.complete_replay().await?);
    tx.commit().await?;
    Ok(())
}
async fn assert_stale_write_rolled_back(
    result: anyhow::Result<()>,
    tx: Connection<'_, Test>,
    pool: &ConnectionPool<Test>,
) -> anyhow::Result<()> {
    assert!(matches!(
        result.unwrap_err().downcast_ref::<StoreError>(),
        Some(StoreError::HeadChanged)
    ));
    drop(tx);
    assert_eq!(children(&mut pool.connection().await?).await?, 0);
    Ok(())
}

#[tokio::test]
async fn core_preparation_and_apply_are_transactional() -> anyhow::Result<()> {
    let (_core, client, pool) = core_fixture().await?;
    let mut db = pool.connection().await?;
    let first = prepare(&client, &mut db).await?;
    assert!(matches!(first.transition, Transition::Append { .. }));
    for commit in [false, true] {
        first.revalidate(client.as_ref()).await?;
        let mut tx = db.start_transaction().await?;
        first.apply_headers(&mut ReaderDal::new(&mut tx)).await?;
        if commit {
            tx.commit().await?;
        } else {
            drop(tx);
        }
        assert_eq!(
            ReaderDal::new(&mut db).get_highest_block().await?.unwrap().header.height,
            u32::from(commit)
        );
    }
    let mut tx = db.start_transaction().await?;
    let header = ReaderDal::new(&mut tx).header_at(1).await?.unwrap().header;
    child(&mut tx, header).await?;
    let result = first.apply_headers(&mut ReaderDal::new(&mut tx)).await;
    assert_stale_write_rolled_back(result, tx, &pool).await?;
    Ok(())
}
#[tokio::test]
async fn authority_and_retained_origins_recheck_source() -> anyhow::Result<()> {
    let (core, client, pool) = core_fixture().await?;
    let mut db = pool.connection().await?;
    apply_first_and_complete(&client, &mut db).await?;
    let current = prepare(&client, &mut db).await?;
    let dependency = current.snapshot.head.clone();
    let mut tx = db.start_transaction().await?;
    current.require_authority(&mut ReaderDal::new(&mut tx), &[dependency.clone()]).await?;
    let mut replaced = dependency;
    replaced.id += 1;
    assert!(current.require_authority(&mut ReaderDal::new(&mut tx), &[replaced]).await.is_err());
    tx.commit().await?;
    let selected = core.mine(1)?[0];
    let second = prepare(&client, &mut db).await?;
    let block = client.checked_block(&client.canonical_header_at(2).await?).await?;
    let txid = block.txdata[0].compute_txid();
    let future = RetainedProofs::prepare(&client, &[txid]).await?;
    assert!(future.complete_through(1));
    assert!(!future.complete_through(2));
    future.revalidate(&client).await?;
    let mut tx = db.start_transaction().await?;
    assert!(second.require_authority(&mut ReaderDal::new(&mut tx), &[]).await.is_err());
    tx.commit().await?;
    core.rpc::<serde_json::Value>("invalidateblock", &[serde_json::json!(selected)])?;
    assert!(second.revalidate(client.as_ref()).await.is_err());
    assert!(future.revalidate(&client).await.is_err());
    assert!(!RetainedProofs::prepare(&client, &[txid]).await?.complete_through(0));
    assert!(matches!(prepare(&client, &mut db).await?.transition, Transition::Idle));
    Ok(())
}
#[tokio::test]
async fn fatal_depth_stop_persists_without_deleting_head() -> anyhow::Result<()> {
    let (core, client, pool) = core_fixture().await?;
    let mut db = pool.connection().await?;
    apply_first_and_complete(&client, &mut db).await?;
    let head = ReaderDal::new(&mut db).get_highest_block().await?.unwrap();
    core.rpc::<serde_json::Value>("invalidateblock", &[serde_json::json!(head.header.block_hash)])?;
    let stopped = prepare(&client, &mut db).await?;
    assert!(
        matches!(&stopped.transition,Transition::Stop(reason) if reason.contains("OldBranchLimit"))
    );
    let mut tx = db.start_transaction().await?;
    stopped.apply_headers(&mut ReaderDal::new(&mut tx)).await?;
    let first_reason = ReaderDal::new(&mut tx).state().await?.unwrap().stopped_reason;
    ReaderDal::new(&mut tx).stop("different later cause").await?;
    tx.commit().await?;
    let mut restarted = pool.connection().await?;
    let mut reader = ReaderDal::new(&mut restarted);
    assert_eq!(reader.get_highest_block().await?.unwrap(), head);
    let state = reader.state().await?.unwrap();
    assert_eq!(state.stopped_reason, first_reason);
    assert!(!state.replay_ready);
    core.rpc::<serde_json::Value>("stop", &[])?;
    assert!(prepare(&client, &mut restarted).await.is_err());
    assert_eq!(ReaderDal::new(&mut restarted).get_highest_block().await?.unwrap(), head);
    Ok(())
}

#[tokio::test]
async fn core_shallow_rewind_commits_native_deletes_consumer_and_frontier() -> anyhow::Result<()> {
    use std::sync::Arc;

    use via_btc_client::indexer::{BitcoinInscriptionIndexer, WithdrawalScanMode};
    use via_test_utils::bitcoin_core::BitcoinCore;
    use zksync_types::via_wallet::SystemWalletsDetails;
    let core = BitcoinCore::new()?;
    let mut client = core.client()?;
    // Six-block policy runs against actual isolated Core headers. Startup network validation is tested separately.
    Arc::get_mut(&mut client).unwrap().config.network = "bitcoin".into();
    core.mine(101)?;
    let wallets = via_test_utils::utils::test_wallets();
    let mut outputs = serde_json::Map::new();
    outputs.insert(wallets.bridge.to_string(), serde_json::json!(0.0005));
    outputs.insert("data".into(), serde_json::json!("07".repeat(20)));
    let raw: String =
        core.rpc("createrawtransaction", &[serde_json::json!([]), serde_json::json!(outputs)])?;
    let funded: serde_json::Value = core.rpc("fundrawtransaction", &[serde_json::json!(raw)])?;
    let signed: serde_json::Value =
        core.rpc("signrawtransactionwithwallet", &[funded["hex"].clone()])?;
    let _: bitcoin::Txid = core.rpc("sendrawtransaction", &[signed["hex"].clone()])?;
    let selected = core.mine(1)?[0];
    let height: u32 = core.rpc("getblockcount", &[])?;
    let genesis = client.canonical_header_at(0).await?;
    let pool = ConnectionPool::<Test>::constrained_test_pool(1).await;
    let mut db = pool.connection().await?;
    let mut tx = db.start_transaction().await?;
    let mut reader = ReaderDal::new(&mut tx);
    reader
        .initialize(
            "bitcoin",
            genesis.hash,
            Header { height: 0, block_hash: genesis.hash, parent_hash: genesis.parent_hash },
            BlockRef { height, block_hash: selected },
        )
        .await?;
    reader
        .insert_wallets(&SystemWalletsDetails::try_from(wallets.clone())?, WalletSource::Unbound(0))
        .await?;
    let anchor = reader.header_at(0).await?.unwrap();
    reader.complete_consumer(PROTOCOL_CONSUMER, &anchor, &anchor).await?;
    tx.commit().await?;
    let mut indexer = BitcoinInscriptionIndexer::new(client.clone(), Arc::new(wallets))
        .with_withdrawal_mode(WithdrawalScanMode::Ignore);
    for _ in 0..height {
        assert!(transition::process_next(&mut db, &client, &mut indexer, 0.66).await?);
    }
    let old = ReaderDal::new(&mut db).header_at(height).await?.unwrap();
    let frontier = ReaderDal::new(&mut db).state().await?.unwrap().confirmed;
    let hash: Vec<u8> = sqlx::query_scalar("SELECT hash FROM transactions WHERE bitcoin_height=$1")
        .bind(i64::from(height))
        .instrument("parsed_native_deposit")
        .fetch_one(&mut db)
        .await?;
    core.rpc::<serde_json::Value>("invalidateblock", &[serde_json::json!(selected)])?;
    assert!(transition::process_next(&mut db, &client, &mut indexer, 0.66).await?);
    let mut reader = ReaderDal::new(&mut db);
    assert_eq!(reader.get_highest_block().await?.unwrap().header.height, height - 1);
    let consumer = reader.consumer(PROTOCOL_CONSUMER).await?.unwrap();
    assert_eq!(consumer.applied.header.height, height - 1);
    assert_eq!(consumer.oldest_needed, anchor);
    assert_eq!(reader.state().await?.unwrap().confirmed, frontier);
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM transactions WHERE hash=$1)")
            .bind(hash)
            .instrument("rewind_deleted_native_deposit")
            .fetch_one(&mut db)
            .await?
    );
    core.mine(1)?;
    transition::process_next(&mut db, &client, &mut indexer, 0.66).await?;
    assert!(ReaderDal::new(&mut db).header_at(height).await?.unwrap().id > old.id);
    Ok(())
}

fn payload<T>(input: T) -> Payload<T> {
    Payload { input, public_key: vec![], signature: vec![], sender_script: None }
}
async fn main_proof_fixture() -> anyhow::Result<(
    BitcoinCore,
    Arc<BitcoinClient>,
    ConnectionPool<Test>,
    Included<via_btc_client::indexer::replay::ProofInput>,
)> {
    use via_btc_client::types::ProofDAReferenceInput;
    use zksync_types::{L1BatchNumber, H256};
    let (core, client, pool) = core_fixture().await?;
    let mut db = pool.connection().await?;
    apply_first_and_complete(&client, &mut db).await?;
    let mut tx = db.start_transaction().await?;
    let mut reader = ReaderDal::new(&mut tx);
    let wallets = via_test_utils::utils::test_wallets();
    reader
        .insert_wallets(&SystemWalletsDetails::try_from(wallets.clone())?, WalletSource::Unbound(0))
        .await?;
    let block = reader.header_at(1).await?.unwrap();
    let origin = MessageOrigin {
        height: 1,
        hash: block.header.block_hash,
        transaction_index: 1,
        message_index: 0,
        txid: Txid::from_byte_array([2; 32]),
    };
    let mut parent = via_test_utils::inscription::batch_reference(
        MessageOrigin {
            transaction_index: 0,
            txid: Txid::from_byte_array([1; 32]),
            ..origin.clone()
        },
        L1BatchNumber(1),
        H256::repeat_byte(4),
        H256::zero(),
        "fixture",
        "batch",
    );
    parent.payload.sender_script = Some(wallets.sequencer.script_pubkey().into_bytes());
    let reference = ProofDAReferenceInput {
        l1_batch_reveal_txid: parent.origin.txid,
        da_identifier: "fixture".into(),
        blob_id: "proof".into(),
    };
    let proof = via_test_utils::inscription::unsigned_proof(parent, origin, reference);
    tx.commit().await?;
    Ok((core, client, pool, proof))
}
async fn save_main_proof(
    db: &mut Connection<'_, Test>,
    proof: &Included<via_btc_client::indexer::replay::ProofInput>,
) -> anyhow::Result<()> {
    use zksync_dal::CoreDal;
    let mut tx = db.start_transaction().await?;
    let header = ReaderDal::new(&mut tx).header_at(proof.origin.height).await?.unwrap();
    tx.via_votes_dal().save_reader_proof(proof, header.id).await?;
    tx.commit().await?;
    Ok(())
}
async fn registered_main_proof_fixture() -> anyhow::Result<(
    BitcoinCore,
    Arc<BitcoinClient>,
    Connection<'static, Test>,
    Included<via_btc_client::indexer::replay::ProofInput>,
)> {
    let (core, client, pool, proof) = main_proof_fixture().await?;
    let mut db = pool.connection().await?;
    save_main_proof(&mut db, &proof).await?;
    Ok((core, client, db, proof))
}
#[tokio::test]
async fn main_proof_registration_requires_current_header_and_canonical_parent() -> anyhow::Result<()>
{
    use zksync_dal::CoreDal;
    let (core, client, pool, proof) = main_proof_fixture().await?;
    let mut db = pool.connection().await?;
    core.mine(1)?;
    let prepared = prepare(&client, &mut db).await?;
    let mut append = db.start_transaction().await?;
    prepared.apply_headers(&mut ReaderDal::new(&mut append)).await?;
    append.commit().await?;
    for defect in 0..4 {
        let mut input = proof.clone();
        let mut tx = db.start_transaction().await?;
        let id = ReaderDal::new(&mut tx).header_at(1).await?.unwrap().id;
        match defect {
            0 => input.origin.hash = BlockHash::all_zeros(),
            1 => input.payload.input.pubdata.origin.hash = BlockHash::all_zeros(),
            2 => input.payload.input.pubdata.origin.height = 2,
            _ => {}
        }
        assert!(tx
            .via_votes_dal()
            .save_reader_proof(&input, if defect == 3 { id + 100 } else { id })
            .await
            .is_err());
        drop(tx);
        assert!(Test::canonical_proofs(&mut db, 1, ProofScope::Interpretation).await?.is_empty());
    }
    save_main_proof(&mut db, &proof).await?;
    assert_eq!(Test::canonical_proofs(&mut db, 1, ProofScope::Interpretation).await?.len(), 1);
    Ok(())
}
#[tokio::test]
async fn main_proof_replay_is_idempotent_and_changed_payload_leaves_saved_row() -> anyhow::Result<()>
{
    let (_core, _client, mut db, mut proof) = registered_main_proof_fixture().await?;
    let original =
        Test::canonical_proofs(&mut db, 1, ProofScope::Interpretation).await?.remove(0).work;
    save_main_proof(&mut db, &proof).await?;
    proof.payload.signature.push(1);
    assert!(save_main_proof(&mut db, &proof).await.is_err());
    let rows = Test::canonical_proofs(&mut db, 1, ProofScope::Interpretation).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].work.id, original.id);
    assert_eq!(rows[0].work.proof, original.proof);
    Ok(())
}
#[tokio::test]
async fn main_proof_source_coordinate_cannot_be_claimed_by_another_txid() -> anyhow::Result<()> {
    let (_core, _client, mut db, mut proof) = registered_main_proof_fixture().await?;
    proof.origin.txid = Txid::from_byte_array([3; 32]);
    assert!(save_main_proof(&mut db, &proof).await.is_err());
    assert_eq!(Test::canonical_proofs(&mut db, 1, ProofScope::Interpretation).await?.len(), 1);
    Ok(())
}
#[tokio::test]
async fn main_proof_txid_cannot_have_two_canonical_inclusions() -> anyhow::Result<()> {
    let (core, client, mut db, mut proof) = registered_main_proof_fixture().await?;
    core.mine(1)?;
    let prepared = prepare(&client, &mut db).await?;
    let mut tx = db.start_transaction().await?;
    prepared.apply_headers(&mut ReaderDal::new(&mut tx)).await?;
    tx.commit().await?;
    proof.origin.height = 2;
    proof.origin.hash = client.canonical_header_at(2).await?.hash;
    assert!(save_main_proof(&mut db, &proof).await.is_err());
    assert_eq!(Test::canonical_proofs(&mut db, 2, ProofScope::Interpretation).await?.len(), 1);
    Ok(())
}
async fn main_finalization_fixture(
    approved: bool,
) -> anyhow::Result<(BitcoinCore, Arc<BitcoinClient>, ConnectionPool<Test>)> {
    use via_btc_client::types::{ValidatorAttestationInput, Vote};
    use zksync_dal::CoreDal;
    use zksync_types::{btc_inscription_operations::ViaBtcInscriptionRequestType, L1BatchNumber};
    let (core, client, pool, proof) = main_proof_fixture().await?;
    let mut db = pool.connection().await?;
    save_main_proof(&mut db, &proof).await?;
    sqlx::query("INSERT INTO l1_batches(number,timestamp,l1_tx_count,l2_tx_count,bloom,priority_ops_onchain_data,created_at,updated_at,initial_bootloader_heap_content,used_contract_hashes) VALUES(1,0,0,0,'', '{}',NOW(),NOW(),'[]','[]')")
        .instrument("finalization_native_batch").execute(&mut db).await?;
    let kind = ViaBtcInscriptionRequestType::CommitProofOnchain;
    let request = db
        .btc_sender_dal()
        .via_save_btc_inscriptions_request(L1BatchNumber(1), kind.to_string(), vec![], 0)
        .await?;
    let raw = proof.origin.txid.to_byte_array();
    let history = db
        .btc_sender_dal()
        .insert_inscription_request_history(&[5; 32], &raw, request, &[], &[], 0, 1)
        .await?;
    db.btc_sender_dal().confirm_inscription(request, history).await?;
    sqlx::query("INSERT INTO via_l1_batch_inscription_request(l1_batch_number,commit_l1_batch_inscription_id,commit_proof_inscription_id,updated_at) VALUES(1,$1,$1,NOW())")
        .bind(request).instrument("exact_outbound_proof_receipt").execute(&mut db).await?;
    let work = Test::canonical_proofs(&mut db, 1, ProofScope::Interpretation).await?.remove(0).work;
    let vote = Included {
        origin: MessageOrigin {
            txid: Txid::from_byte_array([7; 32]),
            transaction_index: 2,
            ..proof.origin
        },
        payload: Payload {
            input: ValidatorAttestationInput {
                reference_txid: work.proof.origin.txid,
                attestation: if approved { Vote::Ok } else { Vote::NotOk },
            },
            sender_script: Some(
                via_test_utils::utils::test_wallets().verifiers[0].script_pubkey().into_bytes(),
            ),
            public_key: vec![],
            signature: vec![],
        },
    };
    let mut tx = db.start_transaction().await?;
    ReaderDal::new(&mut tx).save_reader_vote(&vote, &work, Network::Regtest).await?;
    tx.commit().await?;
    Ok((core, client, pool))
}
async fn finalize_main(
    db: &mut Connection<'_, Test>,
    client: &BitcoinClient,
) -> anyhow::Result<u64> {
    let prepared = prepare(client, db).await?;
    let mut tx = Test::OWNER.begin(db).await?;
    let saved = ReaderDal::new(&mut tx).finalize_ready_proofs(&prepared, 0.3).await?;
    tx.commit().await?;
    Ok(saved)
}
#[tokio::test]
async fn main_finalization_persists_positive_and_negative_exact_proof_quorum() -> anyhow::Result<()>
{
    use zksync_dal::CoreDal;
    for approved in [true, false] {
        let (_core, client, pool) = main_finalization_fixture(approved).await?;
        let mut db = pool.connection().await?;
        assert_eq!(finalize_main(&mut db, &client).await?, 1);
        assert_eq!(finalize_main(&mut db, &client).await?, 0);
        assert_eq!(
            db.via_blocks_dal().get_canonical_finalization(zksync_types::L1BatchNumber(1)).await?,
            Some(approved)
        );
    }
    Ok(())
}

#[tokio::test]
async fn idle_finalization_never_queries_votes() -> anyhow::Result<()> {
    let (_core, client, pool) = main_finalization_fixture(true).await?;
    let mut db = pool.connection().await?;
    let id = Test::canonical_proofs(&mut db, 1, ProofScope::Finalization).await?[0].work.id;
    assert!(ReaderDal::new(&mut db).reader_votes(1, &[id + 1]).await?.is_empty());
    assert_eq!(ReaderDal::new(&mut db).reader_votes(1, &[id]).await?.len(), 1);
    assert_eq!(finalize_main(&mut db, &client).await?, 1);
    sqlx::query("CREATE TEMP VIEW via_votes AS SELECT * FROM public.via_votes WHERE 1/0=0")
        .instrument("idle_vote_query_trap")
        .execute(&mut db)
        .await?;
    assert_eq!(finalize_main(&mut db, &client).await?, 0);
    let error = ReaderDal::new(&mut db).reader_votes(1, &[id]).await.err().unwrap();
    assert!(format!("{error:#}").contains("bitcoin_canonical_votes"));
    Ok(())
}
#[tokio::test]
async fn main_finalization_waits_for_mature_quorum_current_header_and_exact_receipt(
) -> anyhow::Result<()> {
    for hold in [
        "DELETE FROM via_votes",
        "UPDATE via_btc_reader SET confirmed_number=0,confirmed_hash=(SELECT hash FROM via_btc_headers WHERE number=0)",
        "UPDATE via_btc_proofs SET proof_header_id=proof_header_id+100",
        "UPDATE via_btc_inscriptions_request_history SET reveal_tx_id=decode(repeat('09',32),'hex')",
    ] {
        let (_core, client, pool) = main_finalization_fixture(true).await?;
        let mut db = pool.connection().await?;
        sqlx::query(hold).instrument("main_finalization_hold").execute(&mut db).await?;
        assert_eq!(finalize_main(&mut db, &client).await?, 0, "{hold}");
        let count:i64 = sqlx::query_scalar("SELECT count(*) FROM via_btc_finalizations").instrument("held_main_finalizations").fetch_one(&mut db).await?;
        assert_eq!(count,0,"{hold}");
    }
    Ok(())
}
#[tokio::test]
async fn prepared_frontier_change_rolls_back_staged_child() -> anyhow::Result<()> {
    let (_core, client, pool) = core_fixture().await?;
    let mut db = pool.connection().await?;
    apply_first_and_complete(&client, &mut db).await?;
    sqlx::query("UPDATE via_btc_reader SET confirmed_number=0,confirmed_hash=(SELECT hash FROM via_btc_headers WHERE number=0)")
        .instrument("prepare_before_frontier_advances").execute(&mut db).await?;
    let prepared = prepare(&client, &mut db).await?;
    let mut concurrent = db.start_transaction().await?;
    let head = ReaderDal::new(&mut concurrent).header_at(1).await?.unwrap();
    ReaderDal::new(&mut concurrent).advance_frontier(&head, &head).await?;
    concurrent.commit().await?;
    let mut tx = db.start_transaction().await?;
    child(&mut tx, prepared.snapshot.head.header).await?;
    let result = prepared.apply_headers(&mut ReaderDal::new(&mut tx)).await;
    assert_stale_write_rolled_back(result, tx, &pool).await?;
    Ok(())
}
#[tokio::test]
async fn pubdata_authorization_uses_its_own_historical_sequencer() -> anyhow::Result<()> {
    let (_core, _client, pool, mut proof) = main_proof_fixture().await?;
    let mut tx = pool.start_transaction().await?;
    let wallets = via_test_utils::utils::test_wallets();
    let change = Included {
        origin: MessageOrigin { transaction_index: 3, ..proof.origin.clone() },
        payload: payload(ConfigurationInput::Sequencer(
            via_btc_client::types::UpdateSequencerInput {
                inputs: vec![],
                address: wallets.governance.as_unchecked().clone(),
            },
        )),
    };
    let mut reader = ReaderDal::new(&mut tx);
    let id = reader.save_configuration(&change).await?;
    reader
        .decide_configuration(
            id,
            true,
            &via_btc_client::indexer::configuration::project_wallets(
                &change.payload.input,
                change.origin.txid,
                Network::Regtest,
            )?,
        )
        .await?;
    assert!(
        protocol::ProtocolBlock::proof_is_authorized(&mut reader, &proof, Network::Regtest).await?
    );
    proof.payload.input.pubdata.payload.sender_script =
        Some(wallets.governance.script_pubkey().into_bytes());
    assert!(
        !protocol::ProtocolBlock::proof_is_authorized(&mut reader, &proof, Network::Regtest)
            .await?
    );
    Ok(())
}

fn protocol_candidate(
    origin: MessageOrigin,
    minor: zksync_types::ProtocolVersionId,
) -> Included<ConfigurationInput> {
    use via_btc_client::types::{SystemContractUpgradeInput, SystemContractUpgradeProposalInput};
    use zksync_types::{
        protocol_version::{ProtocolSemanticVersion, VersionPatch},
        H256,
    };
    Included {
        origin: origin.clone(),
        payload: payload(ConfigurationInput::Protocol(
            SystemContractUpgradeInput {
                inputs: vec![],
                proposal_tx_id: Txid::from_byte_array([9; 32]),
            },
            vec![Included {
                origin: MessageOrigin { txid: Txid::from_byte_array([9; 32]), ..origin },
                payload: payload(SystemContractUpgradeProposalInput {
                    version: ProtocolSemanticVersion::new(minor, VersionPatch(0)),
                    bootloader_code_hash: H256::repeat_byte(1),
                    default_account_code_hash: H256::repeat_byte(2),
                    evm_emulator_code_hash: None,
                    recursion_scheduler_level_vk_hash: H256::repeat_byte(3),
                    system_contracts: vec![],
                }),
            }],
        )),
    }
}
async fn role_fixture<DB: ReaderRole>() -> anyhow::Result<Connection<'static, DB>> {
    let chain = Chain::with_height(2);
    let pool = ConnectionPool::<DB>::test_pool().await;
    let mut tx = pool.start_transaction().await?;
    assert!(via_test_utils::reader_at(&mut tx, &chain, 2, 0, Network::Regtest).await?);
    ReaderDal::new(&mut tx)
        .insert_wallets(
            &SystemWalletsDetails::try_from(via_test_utils::utils::test_wallets())?,
            WalletSource::Unbound(0),
        )
        .await?;
    sqlx::query("UPDATE via_btc_reader SET replay_ready=FALSE")
        .instrument("partial_protocol_fixture")
        .execute(&mut tx)
        .await?;
    Ok(tx)
}
async fn main_protocol_fixture() -> anyhow::Result<Connection<'static, Test>> {
    use zksync_dal::CoreDal;
    let mut tx = role_fixture::<Test>().await?;
    let mut previous = zksync_types::ProtocolVersion::default();
    previous.version.minor = zksync_types::ProtocolVersionId::Version27;
    tx.protocol_versions_dal().save_protocol_version_with_tx(&previous).await?;
    sqlx::query("UPDATE protocol_patches SET bitcoin_height=0,bitcoin_hash=(SELECT hash FROM via_btc_headers WHERE number=0)").instrument("main_bootstrap_patch").execute(&mut tx).await?;
    Ok(tx)
}
async fn verifier_protocol_fixture(
) -> anyhow::Result<Connection<'static, via_verifier_dal::Verifier>> {
    use via_verifier_dal::VerifierDal;
    use zksync_types::{
        protocol_version::{ProtocolSemanticVersion, VersionPatch},
        ProtocolVersionId,
    };
    let mut tx = role_fixture::<via_verifier_dal::Verifier>().await?;
    tx.via_protocol_versions_dal()
        .save_protocol_version(
            ProtocolSemanticVersion::new(ProtocolVersionId::Version27, VersionPatch(0)),
            &[0; 32],
            &[0; 32],
            &[0; 32],
            &[0; 32],
        )
        .await?;
    for query in ["UPDATE protocol_versions SET executed=TRUE,bitcoin_height=0,bitcoin_hash=(SELECT hash FROM via_btc_headers WHERE number=0)","UPDATE protocol_patches SET bitcoin_height=0,bitcoin_hash=(SELECT hash FROM via_btc_headers WHERE number=0)"] {
        sqlx::query(query).instrument("verifier_bootstrap_protocol").execute(&mut tx).await?;
    }
    Ok(tx)
}
async fn protocol_at_first_header<DB: ReaderRole>(
    tx: &mut Connection<'_, DB>,
    minor: zksync_types::ProtocolVersionId,
) -> anyhow::Result<Included<ConfigurationInput>> {
    let mut reader = ReaderDal::new(tx);
    let header = reader.header_at(1).await?.unwrap();
    Ok(protocol_candidate(
        MessageOrigin {
            height: 1,
            hash: header.header.block_hash,
            transaction_index: 0,
            message_index: 0,
            txid: Txid::from_byte_array([8; 32]),
        },
        minor,
    ))
}
async fn accepted_protocol<DB: ReaderRole>(
    tx: &mut Connection<'_, DB>,
) -> anyhow::Result<StoredConfiguration> {
    let input = protocol_at_first_header(tx, zksync_types::ProtocolVersionId::Version28).await?;
    let mut reader = ReaderDal::new(tx);
    reader.save_configuration(&input).await?;
    Ok(reader.decide_configurations(1, &[], &[], true, Network::Regtest, 0.66).await?.remove(0))
}
async fn native_protocol_binding<DB: ReaderRole>(
    tx: &mut Connection<'_, DB>,
) -> anyhow::Result<StoredConfiguration> {
    let change = accepted_protocol(tx).await?;
    let mut reader = ReaderDal::new(tx);
    assert_eq!(
        reader.protocol_before(1).await?.unwrap().minor,
        zksync_types::ProtocolVersionId::Version27
    );
    assert_eq!(
        reader.protocol_before(2).await?.unwrap().minor,
        zksync_types::ProtocolVersionId::Version28
    );
    assert!(reader.pending_protocol_changes(0).await?.is_empty());
    assert_eq!(reader.pending_protocol_changes(1).await?.len(), 1);
    DB::materialize_protocol(tx, &change).await?;
    assert!(ReaderDal::new(tx).pending_protocol_changes(2).await?.is_empty());
    let bound: (i64,String,serde_json::Value)=sqlx::query_as("SELECT bitcoin_height,bitcoin_hash,bitcoin_payload FROM protocol_patches WHERE minor=28 AND patch=0")
        .instrument("native_bound_protocol").fetch_all(tx).await?.remove(0);
    assert_eq!(
        bound,
        (1, change.change.origin.hash.to_string(), serde_json::to_value(&change.change.payload)?)
    );
    Ok(change)
}
#[tokio::test]
async fn main_protocol_interpretation_precedes_native_binding_and_clears_backlog(
) -> anyhow::Result<()> {
    let mut tx = main_protocol_fixture().await?;
    let change = native_protocol_binding(&mut tx).await?;
    let expected = zksync_types::via_protocol_upgrade::ViaProtocolUpgrade::default()
        .get_canonical_tx_hash(
            zksync_types::protocol_version::ProtocolSemanticVersion::new(
                zksync_types::ProtocolVersionId::Version28,
                zksync_types::protocol_version::VersionPatch(0),
            ),
            vec![],
        )?;
    let hash: Vec<u8> =
        sqlx::query_scalar("SELECT upgrade_tx_hash FROM protocol_versions WHERE id=28")
            .instrument("main_bound_upgrade")
            .fetch_one(&mut tx)
            .await?;
    assert_eq!(hash, expected.as_bytes());
    assert_eq!(change.accepted, Some(true));
    Ok(())
}
#[tokio::test]
async fn verifier_protocol_binding_records_minor_and_patch_sources() -> anyhow::Result<()> {
    let mut tx = verifier_protocol_fixture().await?;
    let change = native_protocol_binding(&mut tx).await?;
    let source: (i64, String, serde_json::Value) = sqlx::query_as(
        "SELECT bitcoin_height,bitcoin_hash,bitcoin_payload FROM protocol_versions WHERE id=28",
    )
    .instrument("verifier_bound_minor")
    .fetch_all(&mut tx)
    .await?
    .remove(0);
    assert_eq!(
        source,
        (1, change.change.origin.hash.to_string(), serde_json::to_value(&change.change.payload)?)
    );
    Ok(())
}
async fn protocol_remine<DB: ReaderRole>(tx: &mut Connection<'_, DB>) -> anyhow::Result<()> {
    let old = native_protocol_binding(tx).await?;
    if DB::NAME == "verifier" {
        sqlx::query("UPDATE protocol_versions SET executed=TRUE WHERE id=28")
            .instrument("executed_before_remine")
            .execute(tx)
            .await?;
    }
    let mut input = old.change;
    input.origin.height = 2;
    input.origin.hash = ReaderDal::new(tx).header_at(2).await?.unwrap().header.block_hash;
    let mut reader = ReaderDal::new(tx);
    let id = reader.save_configuration(&input).await?;
    reader.decide_configuration(id, true, &SystemWalletsDetails::default()).await?;
    let change = reader.configurations_at(2).await?.remove(0);
    DB::materialize_protocol(tx, &change).await?;
    let source: (i64, String) = sqlx::query_as(
        "SELECT bitcoin_height,bitcoin_hash FROM protocol_patches WHERE minor=28 AND patch=0",
    )
    .instrument("remine_bound_patch")
    .fetch_all(tx)
    .await?
    .remove(0);
    assert_eq!(source, (2, input.origin.hash.to_string()));
    let mut changed = change;
    if let ConfigurationInput::Protocol(_, proposals) = &mut changed.change.payload.input {
        proposals[0].payload.input.bootloader_code_hash = zksync_types::H256::repeat_byte(9);
    }
    assert!(DB::materialize_protocol(tx, &changed).await.is_err());
    Ok(())
}
#[tokio::test]
async fn main_protocol_remine_preserves_upgrade_identity_and_rejects_payload_change(
) -> anyhow::Result<()> {
    let mut tx = main_protocol_fixture().await?;
    protocol_remine(&mut tx).await
}
#[tokio::test]
async fn verifier_protocol_remine_preserves_execution_and_rejects_payload_change(
) -> anyhow::Result<()> {
    let mut tx = verifier_protocol_fixture().await?;
    protocol_remine(&mut tx).await?;
    let executed: bool = sqlx::query_scalar("SELECT executed FROM protocol_versions WHERE id=28")
        .instrument("remine_keeps_execution")
        .fetch_one(&mut tx)
        .await?;
    assert!(executed);
    Ok(())
}
#[tokio::test]
async fn protocol_semantic_version_must_increase() -> anyhow::Result<()> {
    for minor in
        [zksync_types::ProtocolVersionId::Version26, zksync_types::ProtocolVersionId::Version27]
    {
        let mut tx = main_protocol_fixture().await?;
        let input = protocol_at_first_header(&mut tx, minor).await?;
        let mut reader = ReaderDal::new(&mut tx);
        reader.save_configuration(&input).await?;
        assert!(reader
            .decide_configurations(1, &[], &[], true, Network::Regtest, 0.66)
            .await?
            .is_empty());
        assert_eq!(reader.configurations_at(1).await?[0].accepted, Some(false));
    }
    Ok(())
}

async fn readiness_reconciles<DB: ReaderRole>(
    tx: &mut Connection<'_, DB>,
    broken: &str,
    reconciled: &str,
    expected_source: Option<String>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE via_btc_reader SET replay_ready=FALSE")
        .instrument("begin_retained_replay")
        .execute(tx)
        .await?;
    sqlx::query(broken).instrument("unreconciled_retained_source").execute(tx).await?;
    assert!(!ReaderDal::new(tx).complete_replay().await?, "{broken}");
    assert_eq!(DB::unreconciled_source(tx).await?, expected_source, "{broken}");
    assert_eq!(metrics::METRICS.replay_unreconciled[&DB::NAME].get(), 1);
    sqlx::query(reconciled).instrument("reconciled_retained_source").execute(tx).await?;
    assert!(ReaderDal::new(tx).complete_replay().await?, "{reconciled}");
    assert!(DB::unreconciled_source(tx).await?.is_none());
    assert_eq!(metrics::METRICS.replay_unreconciled[&DB::NAME].get(), 0);
    Ok(())
}
#[tokio::test]
async fn main_used_protocol_without_bound_patch_holds_replay() -> anyhow::Result<()> {
    let mut tx = main_protocol_fixture().await?;
    sqlx::query("INSERT INTO l1_batches(number,timestamp,l1_tx_count,l2_tx_count,bloom,priority_ops_onchain_data,created_at,updated_at,initial_bootloader_heap_content,used_contract_hashes,protocol_version) VALUES(1,0,0,0,'','{}',NOW(),NOW(),'[]','[]',27)").instrument("retained_main_protocol_use").execute(&mut tx).await?;
    readiness_reconciles(&mut tx,"UPDATE protocol_patches SET bitcoin_hash='orphan'","UPDATE protocol_patches SET bitcoin_hash=(SELECT hash FROM via_btc_headers WHERE number=0)", Some("protocol_minor:27".into())).await
}
#[tokio::test]
async fn verifier_executed_protocol_requires_bound_patch_and_minor() -> anyhow::Result<()> {
    for table in ["protocol_patches", "protocol_versions"] {
        let mut tx = verifier_protocol_fixture().await?;
        readiness_reconciles(
            &mut tx,
            &format!("UPDATE {table} SET bitcoin_hash='orphan'"),
            &format!(
                "UPDATE {table} SET bitcoin_hash=(SELECT hash FROM via_btc_headers WHERE number=0)"
            ),
            Some("protocol_minor:27".into()),
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn verifier_suffix_keeps_protocol_evidence_without_votable_history() -> anyhow::Result<()> {
    let mut tx = verifier_protocol_fixture().await?;
    for query in [
        "UPDATE protocol_versions SET executed=FALSE,bitcoin_height=1",
        "UPDATE protocol_patches SET bitcoin_height=1",
    ] {
        sqlx::query(query).instrument("unexecuted_protocol_evidence").execute(&mut tx).await?;
    }
    via_verifier_dal::Verifier::delete_suffix(&mut tx, 0).await?;
    let retained: (bool, bool) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM protocol_versions WHERE id=27),
        EXISTS(SELECT 1 FROM protocol_patches WHERE minor=27)",
    )
    .instrument("retained_protocol_evidence")
    .fetch_all(&mut tx)
    .await?
    .remove(0);
    assert_eq!(retained, (true, true));
    Ok(())
}
#[tokio::test]
async fn main_executed_deposit_without_canonical_source_holds_replay() -> anyhow::Result<()> {
    use zksync_dal::CoreDal;
    use zksync_types::{l1::via_l1::ViaL1Deposit, Address, H256};
    let mut tx = main_protocol_fixture().await?;
    let hash = ReaderDal::new(&mut tx).header_at(1).await?.unwrap().header.block_hash;
    let operation = ViaL1Deposit {
        l2_receiver_address: Address::repeat_byte(7),
        amount: 50_000,
        calldata: vec![],
        l1_block_number: 1,
        tx_index: 0,
        output_vout: 0,
    }
    .l1_tx()
    .unwrap();
    let deposit = payload((
        via_btc_client::types::L1ToL2MessageInput {
            receiver_l2_address: Address::repeat_byte(7),
            l2_contract_address: Address::zero(),
            call_data: vec![],
        },
        50_000,
    ));
    tx.via_transactions_dal()
        .insert_transaction_l1(
            &operation,
            operation.eth_block(),
            H256::repeat_byte(8),
            &via_btc_client::indexer::replay::DepositOrigin { height: 1, hash, vout: 0 },
            &deposit,
        )
        .await?;
    sqlx::query("UPDATE transactions SET miniblock_number=1")
        .instrument("retained_main_execution")
        .execute(&mut tx)
        .await?;
    readiness_reconciles(
        &mut tx,
        "UPDATE transactions SET bitcoin_hash='orphan'",
        "UPDATE transactions SET bitcoin_hash=(SELECT hash FROM via_btc_headers WHERE number=1)",
        Some(format!("deposit:{:x}", operation.hash())),
    )
    .await
}
#[tokio::test]
async fn verifier_assigned_or_completed_deposit_requires_canonical_source() -> anyhow::Result<()> {
    for state in ["NULL", "TRUE"] {
        let mut tx = verifier_protocol_fixture().await?;
        sqlx::query(&format!("INSERT INTO via_transactions(priority_id,tx_id,receiver,value,canonical_tx_hash,l1_batch_number,status,bitcoin_height,bitcoin_hash,bitcoin_payload) VALUES(1,decode(repeat('01',32),'hex'),'0x07',50000,decode(repeat('02',32),'hex'),1,{state},1,'orphan','{{}}')"))
            .instrument("retained_verifier_deposit").execute(&mut tx).await?;
        readiness_reconciles(&mut tx,"UPDATE via_transactions SET bitcoin_payload=NULL","UPDATE via_transactions SET bitcoin_payload='{}',bitcoin_hash=(SELECT hash FROM via_btc_headers WHERE number=1)", Some(format!("deposit:{}", "02".repeat(32)))).await?;
    }
    Ok(())
}
#[tokio::test]
async fn main_retained_receipt_requires_matching_canonical_proof_hash() -> anyhow::Result<()> {
    let (_core, _client, pool) = main_finalization_fixture(true).await?;
    let mut tx = pool.start_transaction().await?;
    readiness_reconciles(
        &mut tx,
        "UPDATE via_btc_proofs SET block_hash='orphan'",
        "UPDATE via_btc_proofs SET block_hash=(SELECT hash FROM via_btc_headers WHERE number=1)",
        None,
    )
    .await
}
#[tokio::test]
async fn verifier_retained_proof_requires_binding_and_matching_hash() -> anyhow::Result<()> {
    use via_btc_client::types::ProofDAReferenceInput;
    use via_verifier_dal::{Verifier, VerifierDal};
    use zksync_types::{L1BatchNumber, H256};
    for broken in [
        "UPDATE via_votable_transactions SET proof_header_id=NULL,bitcoin_payload=NULL",
        "UPDATE via_votable_transactions SET proof_l1_block_hash='orphan'",
    ] {
        let mut tx = verifier_protocol_fixture().await?;
        let block = ReaderDal::new(&mut tx).header_at(1).await?.unwrap();
        let origin = MessageOrigin {
            height: 1,
            hash: block.header.block_hash,
            transaction_index: 0,
            message_index: 0,
            txid: Txid::from_byte_array([1; 32]),
        };
        let parent = via_test_utils::inscription::batch_reference(
            origin.clone(),
            L1BatchNumber(1),
            H256::repeat_byte(4),
            H256::zero(),
            "fixture",
            "batch",
        );
        let proof = via_test_utils::inscription::unsigned_proof(
            parent,
            MessageOrigin { txid: Txid::from_byte_array([2; 32]), transaction_index: 1, ..origin },
            ProofDAReferenceInput {
                l1_batch_reveal_txid: Txid::from_byte_array([1; 32]),
                da_identifier: "fixture".into(),
                blob_id: "proof".into(),
            },
        );
        tx.via_votes_dal().save_reader_proof(&proof, block.id).await?;
        let raw = via_btc_client::indexer::replay::transaction_hash(proof.origin.txid);
        let id = tx.via_votes_dal().get_votable_transaction_id(raw.as_bytes()).await?.unwrap();
        sqlx::query("UPDATE via_votable_transactions SET l1_batch_status=TRUE")
            .instrument("retained_proof_verdict")
            .execute(&mut tx)
            .await?;
        sqlx::query(broken).instrument("unbound_retained_proof").execute(&mut tx).await?;
        assert_eq!(Verifier::unreconciled_source(&mut tx).await?, Some(format!("votable:{id}")));
        assert!(!ReaderDal::new(&mut tx).complete_replay().await?);
        tx.via_votes_dal().save_reader_proof(&proof, block.id).await?;
        assert!(Verifier::unreconciled_source(&mut tx).await?.is_none());
        assert!(ReaderDal::new(&mut tx).complete_replay().await?);
    }
    Ok(())
}

async fn proposal_fixture(
    kind: &str,
) -> anyhow::Result<(
    via_test_utils::bitcoin_rpc::BitcoinRpcServer,
    BitcoinClient,
    via_btc_client::types::FullInscriptionMessage,
)> {
    use bitcoin::{Amount, ScriptBuf, TxOut, Witness};
    use via_btc_client::{
        inscriber::test_utils::{get_mock_inscriber_and_conditions, MockBitcoinOps},
        types::*,
    };
    let mut chain = Chain::with_height(2);
    let successor = chain.blocks[&2].clone();
    let origin = MessageOrigin {
        height: 1,
        hash: chain.blocks[&1].block_hash(),
        transaction_index: 0,
        message_index: 0,
        txid: Txid::all_zeros(),
    };
    let ConfigurationInput::Protocol(_, proposal) =
        protocol_candidate(origin, zksync_types::ProtocolVersionId::Version28).payload.input
    else {
        unreachable!()
    };
    let message = if kind == "bridge" {
        InscriptionMessage::UpdateBridgeProposal(UpdateBridgeProposalInput {
            bridge_musig2_address: via_test_utils::utils::test_wallets()
                .bridge
                .as_unchecked()
                .clone(),
            verifier_p2wpkh_addresses: via_test_utils::utils::test_wallets()
                .verifiers
                .into_iter()
                .map(|address| address.as_unchecked().clone())
                .collect(),
        })
    } else {
        InscriptionMessage::SystemContractUpgradeProposal(proposal[0].payload.input.clone())
    };
    let mut inscription = get_mock_inscriber_and_conditions(MockBitcoinOps::default())
        .prepare_inscribe(&message, None)
        .await?
        .final_reveal_tx
        .tx;
    if kind == "ambiguous" {
        let mut second = inscription.input[1].clone();
        second.previous_output.vout += 1;
        inscription.input.push(second);
    }
    let txid = inscription.compute_txid();
    let mut block = chain.blocks[&1].clone();
    block.txdata.push(inscription.clone());
    block.txdata[0].input[0].witness = Witness::from_slice(&[[0u8; 32]]);
    let commitment =
        bitcoin::Block::compute_witness_commitment(&block.witness_root().unwrap(), &[0; 32]);
    block.txdata[0].output.push(TxOut {
        value: Amount::ZERO,
        script_pubkey: ScriptBuf::from_bytes(
            [vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed], commitment.to_byte_array().to_vec()]
                .concat(),
        ),
    });
    let hash = chain.insert(1, block);
    let approval_hash = chain.insert(2, successor);
    let verbose = serde_json::json!({"hex":bitcoin::consensus::encode::serialize_hex(&inscription),"txid":txid,"hash":inscription.compute_wtxid(),"size":inscription.total_size(),"vsize":inscription.vsize(),"version":2,"locktime":0,"vin":[],"vout":[],"blockhash":if kind=="missing" {None} else {Some(hash)},"confirmations":1});
    let server = via_test_utils::bitcoin_rpc::BitcoinRpcServer::new_with_errors(
        None,
        true,
        move |_, request| {
            if request["method"] == "getrawtransaction" && request["params"][1] == true {
                Ok(verbose.clone())
            } else {
                chain.rpc(&request)
            }
        },
    )?;
    let client = BitcoinClient::new(
        &server.url(""),
        NodeAuth::None,
        zksync_config::configs::via_btc_client::ViaBtcClientConfig::for_tests(),
    )?;
    let mut common = via_btc_client::indexer::MessageParser::new(Network::Regtest)
        .parse_system_transaction(&inscription, 2, None)
        .remove(0)
        .common()
        .clone();
    common.block_hash = Some(approval_hash);
    common.tx_index = Some(0);
    common.tx_id = Txid::from_byte_array([8; 32]);
    let approval = FullInscriptionMessage::SystemContractUpgrade(SystemContractUpgrade {
        common,
        input: SystemContractUpgradeInput { inputs: vec![], proposal_tx_id: txid },
    });
    Ok((server, client, approval))
}
#[tokio::test]
async fn protocol_declaration_requires_one_canonical_proposal_inside_its_prefix(
) -> anyhow::Result<()> {
    use via_btc_client::indexer::configuration::prepare_configurations;
    for kind in ["valid", "missing", "bridge", "ambiguous", "future"] {
        let (_server, client, mut approval) = proposal_fixture(kind).await?;
        if kind == "future" {
            approval.common_mut().block_height = 0;
        }
        let prepared = prepare_configurations(&client, &[approval]).await;
        if kind == "missing" || kind == "future" {
            assert!(prepared.is_err(), "{kind}");
        } else {
            assert_eq!(prepared?[0].payload.input.valid_proposal(), kind == "valid", "{kind}");
        }
    }
    Ok(())
}
#[tokio::test]
async fn bridge_proposal_for_another_network_cannot_activate() -> anyhow::Result<()> {
    use via_btc_client::{
        indexer::configuration::{prepare_configurations, project_wallets},
        types::*,
    };
    let (_server, client, approval) = proposal_fixture("bridge").await?;
    let FullInscriptionMessage::SystemContractUpgrade(upgrade) = approval else { unreachable!() };
    let bridge = FullInscriptionMessage::UpdateBridge(UpdateBridge {
        common: upgrade.common,
        input: UpdateBridgeInput { inputs: vec![], proposal_tx_id: upgrade.input.proposal_tx_id },
    });
    let input = prepare_configurations(&client, &[bridge]).await?.remove(0);
    assert!(project_wallets(&input.payload.input, input.origin.txid, Network::Bitcoin).is_err());
    assert!(project_wallets(&input.payload.input, input.origin.txid, Network::Regtest).is_ok());
    Ok(())
}
