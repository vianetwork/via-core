//! Inclusive Bitcoin confirmation arithmetic. Canonicality is checked at the source boundary.

type L1Height = u32;

// Alpen compute_confirmation_depth and is_l1_reorg_safe: functions unchanged; local height alias.
// Copyright (c) 2024 Strata contributors. Licensed under the MIT option (LICENSE-MIT).
// https://github.com/alpenlabs/alpen/blob/882107abd8ec691f870dced0a680267124dd8294/crates/primitives/src/l1/mod.rs
/// Computes how many confirmations an L1 block at `observed_height` has under
/// `current_tip`, counting the block itself as one confirmation.
///
/// A block at the tip has 1 confirmation; one block below the tip has 2; etc.
/// Returns [`None`] when `observed_height` is above `current_tip`, since such a
/// block is not actually confirmed under that tip.
#[rustfmt::skip]
pub fn compute_confirmation_depth(observed_height: L1Height, current_tip: L1Height) -> Option<u32> {
    if observed_height > current_tip {
        return None;
    }
    Some(
        current_tip
            .saturating_sub(observed_height)
            .saturating_add(1),
    )
}

/// A single computation logic for whether an L1 block at `observed_height` is buried deep enough
/// under `current_tip` to be considered reorg-safe.
#[rustfmt::skip]
pub fn is_l1_reorg_safe(
    observed_height: L1Height,
    current_tip: L1Height,
    l1_reorg_safe_depth: u32,
) -> bool {
    compute_confirmation_depth(observed_height, current_tip)
        .is_some_and(|depth| depth >= l1_reorg_safe_depth.max(1))
}
