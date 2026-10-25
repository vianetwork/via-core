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
async fn prepare(
    client: &BitcoinClient,
    db: &mut Connection<'_, Test>,
) -> anyhow::Result<PreparedTransition> {
    let snapshot =
        ReaderSnapshot::load(&mut ReaderDal::new(db), client.reorg_safe_depth()?).await?;
    PreparedTransition::prepare(client, snapshot).await
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
    let error = first.apply_headers(&mut ReaderDal::new(&mut tx)).await.unwrap_err();
    assert!(matches!(error.downcast_ref::<StoreError>(), Some(StoreError::HeadChanged)));
    drop(tx);
    assert_eq!(children(&mut db).await?, 0);
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
    // Exercise the six-block policy against actual isolated Core headers; startup network validation is tested separately.
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
