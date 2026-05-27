// fixtures/reorg/empty_l1_blocks_good.rs
// GOOD pattern - empty via_l1_blocks is treated as bootstrap / inconclusive.

async fn detect_reorg_good(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
    let Some((block_height, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
        tracing::debug!("Skipping reorg check: via_l1_blocks not yet bootstrapped");
        return Ok(());
    };

    let _ = block_height;
    Ok(())
}

async fn detect_reorg_verifier_good(&self, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<bool> {
    let Some((block_height, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
        tracing::debug!("Skipping reorg check: via_l1_blocks not yet bootstrapped");
        return Ok(false);
    };

    let _ = block_height;
    Ok(false)
}

async fn sync_l1_blocks_good(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
    let (block_height, hash) = match storage.via_l1_block_dal().get_last_l1_block().await? {
        Some(row) => row,
        None => match self.lazy_bootstrap_first_l1_block(storage).await? {
            Some(row) => row,
            None => return Ok(()),
        },
    };

    let _ = (block_height, hash);
    Ok(())
}

async fn unrelated_missing_block_is_still_allowed(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
    let Some(block) = storage.via_l1_block_dal().get_block_by_hash("hash").await? else {
        anyhow::bail!("Block not found for hash")
    };

    let _ = block;
    Ok(())
}
