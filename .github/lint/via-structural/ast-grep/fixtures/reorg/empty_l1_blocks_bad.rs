// fixtures/reorg/empty_l1_blocks_bad.rs
// BAD pattern - fatal handling of an empty via_l1_blocks table in reorg detectors.
// Empty via_l1_blocks can be a legitimate bootstrap state when via_btc_watch has
// not yet written its indexer metadata.

async fn detect_reorg_bad(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
    let Some((block_height, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
        anyhow::bail!("No blocks found to detect reorg")
    };

    let _ = block_height;
    Ok(())
}

async fn detect_reorg_verifier_bad(&self, storage: &mut Connection<'_, Verifier>) -> anyhow::Result<bool> {
    let Some((block_height, _)) = storage.via_l1_block_dal().get_last_l1_block().await? else {
        bail!("No blocks found to detect reorg")
    };

    let _ = block_height;
    Ok(false)
}

async fn sync_l1_blocks_bad(&self, storage: &mut Connection<'_, Core>) -> anyhow::Result<()> {
    let (block_height, hash) = match storage.via_l1_block_dal().get_last_l1_block().await? {
        Some(row) => row,
        None => anyhow::bail!("No blocks found to sync blocks"),
    };

    let _ = (block_height, hash);
    Ok(())
}
