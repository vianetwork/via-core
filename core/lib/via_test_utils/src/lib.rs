pub mod bitcoin_core;
pub mod bitcoin_rpc;
pub mod chain;
pub mod inscription;
pub mod utils;

/// reader_at builds shared reader prefix up to height and returns actual replay readiness.
/// Callers keep role-specific evidence and deliberate partial-replay states in own fixtures.
/// Idea only from LDK Blockchain::with_height, which builds linked test chain at requested height: https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/test_utils.rs
pub async fn reader_at<DB: via_btc_storage::ReaderRole>(
    storage: &mut zksync_db_connection::connection::Connection<'_, DB>,
    chain: &chain::Chain,
    height: u32,
    confirmed: u32,
    network: bitcoin::Network,
) -> anyhow::Result<bool> {
    use via_btc_storage::ReaderDal;
    let header = |height| via_reorg::chain::Header {
        height,
        block_hash: chain.blocks[&height].block_hash(),
        parent_hash: chain.blocks[&height].header.prev_blockhash,
    };
    let mut reader = ReaderDal::new(storage);
    reader
        .initialize(&network.to_string(), header(0).block_hash, header(0), header(height).into())
        .await?;
    let root = reader.header_at(0).await?.unwrap();
    for number in 0..=height {
        let previous = reader.get_highest_block().await?.unwrap();
        if number > previous.header.height {
            reader.save_block(header(number), &previous).await?;
        }
        let block = reader.header_at(number).await?.unwrap();
        for name in DB::REQUIRED_CONSUMERS {
            if reader
                .consumer(name)
                .await?
                .map_or(true, |cursor| cursor.applied.header.height < number)
            {
                reader.complete_consumer(name, &block, &root).await?;
            }
        }
    }
    let head = reader.get_highest_block().await?.unwrap();
    if confirmed >= reader.state().await?.unwrap().confirmed.height {
        let frontier = reader.header_at(confirmed).await?.unwrap();
        reader.advance_frontier(&head, &frontier).await?;
    }
    reader.complete_replay().await
}
