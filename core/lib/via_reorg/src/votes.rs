//! Callers select canonical origins and the verifier set effective at the proof's inclusion.

use std::{collections::HashSet, hash::Hash};

/// tally counts each historical member once for one exact proof inclusion, preserving Via's ratio and approval-on-tie rule.
/// Conflicting payloads for stored vote identity fail at insertion.
/// Idea only from CometBFT commit verification, which counts distinct voters against historical validator set and uses weighted threshold Via does not: https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/validation.go
pub fn tally<P: Eq, V: Eq + Hash>(
    proof: &P,
    verifiers: &[V],
    votes: impl IntoIterator<Item = (P, V, bool)>,
    threshold: f64,
) -> Option<bool> {
    let mut seen = HashSet::new();
    let mut approvals = 0;
    let mut rejections = 0;
    for (candidate, verifier, approved) in votes {
        if candidate != *proof || !verifiers.contains(&verifier) || !seen.insert(verifier) {
            continue;
        }
        if approved {
            approvals += 1;
        } else {
            rejections += 1;
        }
    }
    let approved = approvals >= rejections;
    let count = approvals.max(rejections);
    ((count as f64) / (verifiers.len() as f64) >= threshold).then_some(approved)
}
