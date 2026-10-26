//! Block-local occurrence policy. Adapters establish the complete end-of-block input prefix.

use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

/// conflicting_roles returns roles with two distinct occurrences in one block.
/// Exact repeat of one occurrence is replay, not conflict, so only differing identities count.
/// Idea only from CometBFT processChanges, which fails validator update set containing duplicate address: https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/validator_set.go
pub fn conflicting_roles<R: Eq + Hash, I: Eq + Hash>(
    changes: impl IntoIterator<Item = (R, I)>,
) -> HashSet<R> {
    let mut occurrences: HashMap<R, HashSet<I>> = HashMap::new();
    for (role, identity) in changes {
        occurrences.entry(role).or_default().insert(identity);
    }
    occurrences.into_iter().filter_map(|(role, ids)| (ids.len() > 1).then_some(role)).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationDecision {
    NotReady,
    Accepted,
    Rejected,
}

/// Later votes cannot rescue this frozen prefix.
pub fn decide_rotation(quorums: impl IntoIterator<Item = Option<bool>>) -> RotationDecision {
    if quorums.into_iter().all(|quorum| quorum.is_some()) {
        RotationDecision::Accepted
    } else {
        RotationDecision::Rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_conflicts_use_occurrence_identity() {
        assert!(conflicting_roles([("bridge", 7), ("bridge", 7)]).is_empty());
        assert_eq!(
            conflicting_roles([("bridge", 7), ("bridge", 8), ("sequencer", 9)]),
            HashSet::from(["bridge"])
        );
    }

    #[test]
    fn rotation_decision_requires_prefix_quorums() {
        assert_eq!(decide_rotation([Some(true), None]), RotationDecision::Rejected);
        assert_eq!(decide_rotation([Some(true), Some(false)]), RotationDecision::Accepted);
    }
}
