use std::collections::HashMap;

/// Returns the lowest height in `start_height..start_height + chain_hashes.len()` where
/// `db_blocks` records a block hash that differs from the canonical chain hash.
///
/// `db_blocks` is allowed to be sparse: heights present on the canonical chain but absent
/// in `db_blocks` are ignored instead of being treated as divergences. This keeps a
/// freshly-bootstrapped detector (whose `via_l1_blocks` may only hold the wallet bootstrap
/// row) from falsely reporting a reorg by comparing rows positionally.
pub(crate) fn find_reorg_start_height(
    start_height: i64,
    chain_hashes: &[String],
    db_blocks: &[(i64, String)],
) -> Option<i64> {
    let db: HashMap<i64, &str> = db_blocks.iter().map(|(n, h)| (*n, h.as_str())).collect();
    for (i, chain_hash) in chain_hashes.iter().enumerate() {
        let height = start_height + i as i64;
        if let Some(db_hash) = db.get(&height) {
            if chain_hash != db_hash {
                return Some(height);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn sparse_db_with_only_bootstrap_row_does_not_trigger_reorg() {
        let start_height = 100_792;
        let chain_hashes: Vec<String> = (start_height..=100_891)
            .map(|n| format!("canonical-{n}"))
            .collect();
        let db_blocks = vec![(100_891, h("canonical-100891"))];

        assert_eq!(
            find_reorg_start_height(start_height, &chain_hashes, &db_blocks),
            None
        );
    }

    #[test]
    fn empty_db_does_not_trigger_reorg() {
        let chain_hashes = vec![h("a"), h("b"), h("c")];
        assert_eq!(find_reorg_start_height(10, &chain_hashes, &[]), None);
    }

    #[test]
    fn matching_dense_window_does_not_trigger_reorg() {
        let chain_hashes = vec![h("a"), h("b"), h("c")];
        let db_blocks = vec![(10, h("a")), (11, h("b")), (12, h("c"))];
        assert_eq!(find_reorg_start_height(10, &chain_hashes, &db_blocks), None);
    }

    #[test]
    fn divergence_is_reported_by_height_not_position() {
        let chain_hashes = vec![h("a"), h("b"), h("c")];
        let db_blocks = vec![(12, h("c-other"))];
        assert_eq!(
            find_reorg_start_height(10, &chain_hashes, &db_blocks),
            Some(12)
        );
    }

    #[test]
    fn earliest_divergent_height_is_returned() {
        let chain_hashes = vec![h("a"), h("b"), h("c"), h("d")];
        let db_blocks = vec![
            (10, h("a")),
            (11, h("b-other")),
            (12, h("c")),
            (13, h("d-other")),
        ];
        assert_eq!(
            find_reorg_start_height(10, &chain_hashes, &db_blocks),
            Some(11)
        );
    }
}
