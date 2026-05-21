// fixtures/reorg/zip_bad.rs
// BAD pattern - positional zip after unordered fetch on height-indexed data.
// This is the exact anti-pattern that caused false reorg detections on sparse windows.

async fn detect_reorg_bad(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
    let Some((block_height, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
        anyhow::bail!("No blocks found");
    };

    let window = self.config.reorg_window();
    let start_height = block_height.saturating_sub(window - 1).max(1);

    // DB rows ordered by height
    let db_blocks = storage
        .via_l1_block_dal()
        .list_l1_blocks(start_height, window)
        .await?;

    // Async fetch - order not guaranteed
    let chain_blocks = self.fetch_blocks(start_height, block_height).await?;

    // DANGEROUS: positional zip assumes order is preserved
    for ((db_number, db_hash), chain_block) in db_blocks.iter().zip(chain_blocks.iter()) {
        if chain_block.block_hash().to_string() != *db_hash {
            // false reorg possible here when window is sparse or fetches are out of order
            return Ok(());
        }
    }
    Ok(())
}

async fn fetch_blocks(&self, from: i64, to: i64) -> anyhow::Result<Vec<Block>> {
    // uses buffer_unordered internally
    use futures::stream::{self, StreamExt};
    let heights: Vec<i64> = (from..=to).collect();
    let results = stream::iter(heights)
        .map(|h| async move { self.btc_client.fetch_block(h as u128).await })
        .buffer_unordered(self.config.max_concurrent_fetches())
        .collect::<Vec<_>>()
        .await;
    // ...
}