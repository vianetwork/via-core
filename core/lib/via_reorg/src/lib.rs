//! Shared, dependency-free helpers for Via L1 reorg detectors.
//!
//! Both the main-node detector (`via_main_node_reorg_detector`) and the verifier
//! detector (`via_verifier_reorg_detector`) compare a sparse local view of
//! `via_l1_blocks` against a window of canonical Bitcoin blocks. The detectors
//! use different DB pools, but this comparison depends only on `(height, hash)` pairs.
//!
//! # Invariant
//!
//! Comparison is always by explicit Bitcoin block height. Positional `zip`
//! between DB rows and a fetched canonical window is never sufficient: the local
//! `via_l1_blocks` table can be sparse (e.g. a freshly bootstrapped external node
//! whose only row is the wallet bootstrap block), and the canonical fetch can be
//! incomplete (transient RPC misses). Comparing by position in either case silently
//! aligns the wrong heights and produces false reorgs.

use std::collections::HashMap;

/// Result of comparing local `via_l1_blocks` rows against canonical Bitcoin blocks by height.
///
/// [`Self::SparseAt`] is different from [`Self::ReorgAt`]:
///
/// This is not a reorg. It simply means the comparison could not be completed
/// because canonical data was missing. Do not demote or move the `via_btc_watch`
/// cursor, and do not trigger any reorg handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReorgScan {
    /// All DB heights present in the canonical map matched.
    NoReorg,

    /// First (lowest) height where the DB hash diverges from the canonical hash.
    ReorgAt(i64),

    /// Canonical data is missing for a DB-known height.
    SparseAt(i64),
}

/// Compares local DB rows against a height-keyed canonical map.
///
/// `db_blocks` is expected to be ordered ascending by height (as returned by
/// `via_l1_block_dal::list_l1_blocks`), but matching is done by explicit height,
/// not by position in the lists. This makes sparse `db_blocks` safe by construction.
///
/// Returns the lowest height at which a mismatch was found.
#[must_use]
pub fn scan_for_reorg(db_blocks: &[(i64, String)], canonical_by_height: &HashMap<i64, String>) -> ReorgScan {
    for (db_height, db_hash) in db_blocks {
        match canonical_by_height.get(db_height) {
            Some(canonical_hash) if canonical_hash == db_hash => continue,
            Some(_) => return ReorgScan::ReorgAt(*db_height),
            None => return ReorgScan::SparseAt(*db_height),
        }
    }
    ReorgScan::NoReorg
}

/// Returns the start height of the reorg detection window for a given tip
/// and window size (clamped to at least 1).
///
/// This helper is shared so both the main-node and verifier detectors use
/// identical window calculations.
#[inline]
#[must_use]
pub fn reorg_window_start(tip: i64, window: i64) -> i64 {
    let w = window.max(1);
    tip.saturating_sub(w - 1).max(1)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{reorg_window_start, scan_for_reorg, ReorgScan};

    fn canon(entries: &[(i64, &str)]) -> HashMap<i64, String> {
        entries.iter().map(|(h, s)| (*h, (*s).to_string())).collect()
    }

    #[test]
    fn empty_db_is_no_reorg() {
        let db: Vec<(i64, String)> = vec![];
        let canonical = canon(&[(100_891, "anything")]);
        assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::NoReorg);
    }

    #[test]
    fn matching_dense_window_is_no_reorg() {
        let db = vec![(100_890, "h_890".to_string()), (100_891, "h_891".to_string()), (100_892, "h_892".to_string())];
        let canonical = canon(&[(100_890, "h_890"), (100_891, "h_891"), (100_892, "h_892")]);
        assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::NoReorg);
    }

    /// A sparse DB window with only the wallet bootstrap row must compare that
    /// row against the canonical hash for the same height.
    #[test]
    fn sparse_db_with_only_wallet_bootstrap_row_does_not_falsely_reorg() {
        let mut canonical: HashMap<i64, String> = (100_792..=100_891).map(|h| (h, format!("canonical_{h}"))).collect();
        canonical.insert(100_891, "wallet_bootstrap_hash".to_string());

        let db = vec![(100_891, "wallet_bootstrap_hash".to_string())];

        assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::NoReorg);
    }

    #[test]
    fn divergence_at_known_height_is_reported_as_reorg_at_that_height() {
        let db = vec![
            (100_890, "ok_890".to_string()),
            (100_891, "stale_891".to_string()),
            (100_892, "stale_892".to_string()),
        ];
        let canonical = canon(&[(100_890, "ok_890"), (100_891, "canonical_891"), (100_892, "canonical_892")]);
        assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::ReorgAt(100_891));
    }

    #[test]
    fn first_diverging_db_row_wins() {
        // DB rows are ascending; the helper must surface the lowest diverging
        // height, not some later one.
        let db =
            vec![(100_890, "ok_890".to_string()), (100_891, "bad_891".to_string()), (100_892, "bad_892".to_string())];
        let canonical = canon(&[(100_890, "ok_890"), (100_891, "good_891"), (100_892, "good_892")]);
        assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::ReorgAt(100_891));
    }

    #[test]
    fn missing_canonical_height_for_known_db_row_is_sparse_not_reorg() {
        // Missing canonical data for a DB-known height is inconclusive.
        let db = vec![(100_891, "wallet_bootstrap_hash".to_string())];
        let canonical = canon(&[(100_792, "canonical_792"), (100_793, "canonical_793")]);

        assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::SparseAt(100_891),);
    }

    #[test]
    fn reorg_takes_precedence_over_later_sparse_height() {
        // The earliest diverging row wins over a later inconclusive row.
        let db = vec![(100_890, "stale_890".to_string()), (100_891, "any_891".to_string())];
        let canonical = canon(&[(100_890, "canonical_890")]);
        assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::ReorgAt(100_890));
    }

    #[test]
    fn reorg_window_start_clamps_to_one_and_handles_small_tips() {
        assert_eq!(reorg_window_start(100_891, 100), 100_792);
        assert_eq!(reorg_window_start(50, 100), 1);
        assert_eq!(reorg_window_start(1, 100), 1);
        assert_eq!(reorg_window_start(100_891, 1), 100_891);
        assert_eq!(reorg_window_start(0, 100), 1);
    }
}
