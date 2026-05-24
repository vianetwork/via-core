//! Unit tests for the pure reorg-scan helper.
//!
//! These tests pin down the regression described in the Hetzner private
//! external-node rehearsal: a fresh/sparse `via_l1_blocks` table must not
//! cause the detector to compare a wallet bootstrap block height against
//! the wrong canonical block by positional `zip`.

use std::collections::HashMap;

use super::{scan_for_reorg, ReorgScan};

fn canon(entries: &[(i64, &str)]) -> HashMap<i64, String> {
    entries
        .iter()
        .map(|(h, s)| (*h, (*s).to_string()))
        .collect()
}

#[test]
fn no_reorg_when_db_subset_matches_canonical() {
    let db = vec![(100_891, "hash_891".to_string())];
    let canonical = canon(&[(100_792, "hash_792"), (100_891, "hash_891")]);

    assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::NoReorg);
}

/// Regression: bootstrap inserted exactly one row at the wallet block
/// (e.g. 100_891) while the detector window starts at 100_792. Positional
/// zip would compare DB row index 0 (height 100_891) against canonical
/// chain index 0 (height 100_792) and falsely report a reorg at 100_891.
#[test]
fn sparse_db_does_not_falsely_compare_against_window_start() {
    let db = vec![(100_891, "wallet_bootstrap_hash".to_string())];

    // Canonical chain across the whole 100-block reorg window. All canonical
    // hashes are distinct from the bootstrap hash so a positional zip would
    // have flagged a reorg at 100_891 vs canonical 100_792.
    let canonical: HashMap<i64, String> = (100_792..=100_891)
        .map(|h| (h, format!("canonical_{h}")))
        .collect();
    let mut canonical = canonical;
    // The wallet bootstrap row IS on canonical chain at its own height; the
    // detector must agree.
    canonical.insert(100_891, "wallet_bootstrap_hash".to_string());

    assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::NoReorg);
}

#[test]
fn missing_canonical_height_for_present_db_row_returns_sparse() {
    let db = vec![(100_891, "wallet_bootstrap_hash".to_string())];

    // Simulate a stale/partial fetch where canonical map does not contain
    // the height the DB knows about. We must NOT silently fall back to
    // positional comparison; we must signal sparse.
    let canonical = canon(&[(100_792, "canonical_792"), (100_793, "canonical_793")]);

    assert_eq!(
        scan_for_reorg(&db, &canonical),
        ReorgScan::SparseAt(100_891)
    );
}

#[test]
fn real_reorg_is_reported_at_diverging_height() {
    let db = vec![
        (100_890, "hash_890".to_string()),
        (100_891, "stale_891".to_string()),
        (100_892, "stale_892".to_string()),
    ];
    let canonical = canon(&[
        (100_890, "hash_890"),
        (100_891, "canonical_891"),
        (100_892, "canonical_892"),
    ]);

    assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::ReorgAt(100_891));
}

/// The DB rows are returned ordered ascending by `number`; the helper must
/// pick the lowest diverging height, not the first encountered after a
/// missing one.
#[test]
fn first_diverging_db_row_wins() {
    let db = vec![
        (100_890, "ok_890".to_string()),
        (100_891, "bad_891".to_string()),
        (100_892, "bad_892".to_string()),
    ];
    let canonical = canon(&[
        (100_890, "ok_890"),
        (100_891, "good_891"),
        (100_892, "good_892"),
    ]);

    assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::ReorgAt(100_891));
}

#[test]
fn empty_db_is_no_reorg() {
    let db: Vec<(i64, String)> = vec![];
    let canonical = canon(&[(100_891, "anything")]);
    assert_eq!(scan_for_reorg(&db, &canonical), ReorgScan::NoReorg);
}
