use via_test_utils::chain::Chain;
use zksync_db_connection::connection_pool::ConnectionPool;

use super::*;

#[derive(Debug, Clone)]
struct Test;
impl DbMarker for Test {}

async fn children(connection: &mut Connection<'_, Test>) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM via_btc_configuration")
        .instrument("reader_test_count")
        .fetch_one(connection)
        .await?)
}

#[tokio::test]
async fn aborted_suffix_and_forward_writes_preserve_head_children_and_frontier(
) -> anyhow::Result<()> {
    let chain = Chain::with_height(2);
    let header = |height| Header {
        height,
        block_hash: chain.blocks[&height].block_hash(),
        parent_hash: chain.blocks[&height].header.prev_blockhash,
    };
    let pool = ConnectionPool::<Test>::test_pool().await;
    let mut connection = pool.connection().await?;
    let mut tx = connection.start_transaction().await?;
    let mut reader = ReaderDal::new(&mut tx);
    reader.initialize("regtest", header(0).block_hash, header(0), header(2).into()).await?;
    let genesis = reader.get_highest_block().await?.unwrap();
    tx.commit().await?;

    for commit in [false, true] {
        let mut tx = connection.start_transaction().await?;
        sqlx::query(
            "INSERT INTO via_btc_configuration
            (l1_block_number,block_hash,transaction_index,message_index,txid,role,payload)
            VALUES (1,$1,0,0,'candidate','bridge','{}')",
        )
        .bind(header(1).block_hash.to_string())
        .instrument("reader_test_child")
        .execute(&mut tx)
        .await?;
        ReaderDal::new(&mut tx).save_block(header(1), &genesis).await?;
        if commit {
            tx.commit().await?;
        } else {
            drop(tx);
        }
        let mut fresh = pool.connection().await?;
        assert_eq!(
            ReaderDal::new(&mut fresh).get_highest_block().await?.unwrap().header.height,
            u32::from(commit)
        );
        assert_eq!(children(&mut fresh).await?, i64::from(commit));
    }
    let mut tx = connection.start_transaction().await?;
    let mut reader = ReaderDal::new(&mut tx);
    let first = reader.get_highest_block().await?.unwrap();
    reader.save_block(header(2), &first).await?;
    let second = reader.get_highest_block().await?.unwrap();
    reader.advance_frontier(&second, &first).await?;
    reader.complete_consumer("ordered", &genesis).await?;
    assert!(reader.complete_consumer("ordered", &second).await.is_err());
    reader.complete_consumer("ordered", &first).await?;
    reader.complete_consumer("ordered", &second).await?;
    sqlx::query(
        "INSERT INTO via_btc_configuration
        (l1_block_number,block_hash,transaction_index,message_index,txid,role,payload)
        SELECT 2,$1,transaction_index,message_index,txid,role,payload FROM via_btc_configuration",
    )
    .bind(header(2).block_hash.to_string())
    .instrument("reader_test_second_child")
    .execute(&mut tx)
    .await?;
    sqlx::query("INSERT INTO via_btc_consumers (name,header_id) VALUES ('interpretation',$1)")
        .bind(second.id)
        .instrument("reader_test_consumer")
        .execute(&mut tx)
        .await?;
    tx.commit().await?;

    for commit in [false, true] {
        let mut tx = connection.start_transaction().await?;
        sqlx::query("DELETE FROM via_btc_configuration WHERE l1_block_number>1")
            .instrument("reader_test_delete")
            .execute(&mut tx)
            .await?;
        ReaderDal::new(&mut tx).rewind_headers(&second, &first, header(1).into()).await?;
        if commit {
            tx.commit().await?;
        } else {
            drop(tx);
        }
        let mut fresh = pool.connection().await?;
        let mut reader = ReaderDal::new(&mut fresh);
        assert_eq!(
            reader.get_highest_block().await?.unwrap().header.height,
            if commit { 1 } else { 2 }
        );
        let state = reader.state().await?.unwrap();
        assert_eq!(state.confirmed, header(1).into());
        assert!(!state.replay_ready);
        assert_eq!(children(&mut fresh).await?, if commit { 1 } else { 2 });
    }
    let mut tx = connection.start_transaction().await?;
    let mut reader = ReaderDal::new(&mut tx);
    let failure = reader.rewind_headers(&first, &genesis, header(0).into()).await.unwrap_err();
    assert!(matches!(failure.downcast_ref::<StoreError>(), Some(StoreError::Integrity(_))));
    let reincarnation = reader.save_block(header(2), &first).await?;
    assert!(reincarnation > second.id);
    assert!(!reader.complete_replay(&["interpretation"], "SELECT FALSE").await?);
    sqlx::query("UPDATE via_btc_configuration SET accepted=FALSE")
        .instrument("reader_test_configuration_decided")
        .execute(&mut tx)
        .await?;
    assert!(ReaderDal::new(&mut tx).complete_replay(&["interpretation"], "SELECT FALSE").await?);
    tx.commit().await?;
    Ok(())
}
