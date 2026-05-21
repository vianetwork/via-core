// fixtures/reorg/zip_good.rs
// GOOD pattern - explicit height-based association after unordered fetch.

use std::collections::HashMap;

async fn detect_reorg_good(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
    let Some((block_height, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
        anyhow::bail!("No blocks found");
    };

    let window = self.config.reorg_window();
    let start_height = block_height.saturating_sub(window - 1).max(1);

    let db_blocks = storage
        .via_l1_block_dal()
        .list_l1_blocks(start_height, window)
        .await?;

    let chain_blocks = self.fetch_blocks(start_height, block_height).await?;

    // GOOD: build height -> hash map from DB
    let db_by_height: HashMap<i64, String> = db_blocks.into_iter().collect();

    // Re-associate fetched blocks by explicit height
    for block in chain_blocks {
        // Extract height from block (or request context) to associate with DB data
        let height = block.height();
        if let Some(expected_hash) = db_by_height.get(&height) {
            if block.block_hash().to_string() != *expected_hash {
                // real reorg
            }
        }
    }
    Ok(())
}