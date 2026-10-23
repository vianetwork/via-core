//! Configuration admission depends on the included Bitcoin prefix, never worker timing.

use std::collections::{BTreeMap, BTreeSet};

/// Roles touched by more than one distinct included update. Replaying one identity is harmless.
/// Idea: etcd duplicate-key validation, pinned at
/// <https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/etcdserver/api/v3rpc/key.go#L236-L314>.
pub fn duplicate_roles<R: Ord, I: Ord>(updates: impl IntoIterator<Item = (R, I)>) -> BTreeSet<R> {
    let mut identities = BTreeMap::<R, BTreeSet<I>>::new();
    for (role, identity) in updates {
        identities.entry(role).or_default().insert(identity);
    }
    identities.into_iter().filter_map(|(role, ids)| (ids.len() > 1).then_some(role)).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationAdmission {
    Accepted,
    RejectedEarlierUnfinalizedProof,
    NotReady,
}

/// A rejected included rotation stays rejected; a later quorum requires a new message.
/// Idea: Raft's included pending-configuration guard, pinned at
/// <https://github.com/etcd-io/raft/blob/1931ff5beacc1e2539003c0c70f096285f04c425/raft.go#L1326-L1344>.
pub fn admit_rotation(prefix_complete: bool, earlier_unfinalized_proof: bool) -> RotationAdmission {
    if !prefix_complete {
        RotationAdmission::NotReady
    } else if earlier_unfinalized_proof {
        RotationAdmission::RejectedEarlierUnfinalizedProof
    } else {
        RotationAdmission::Accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_updates_reject_every_duplicate_role_even_when_values_match() {
        // Ported duplicate-key cases: equal values do not turn two writes into one.
        assert_eq!(duplicate_roles([("bridge", 1), ("bridge", 2)]), BTreeSet::from(["bridge"]));
        assert_eq!(duplicate_roles([("bridge", 1), ("bridge", 1)]), BTreeSet::new());
        assert_eq!(duplicate_roles([("bridge", 1), ("sequencer", 2)]), BTreeSet::new());
        assert_eq!(
            duplicate_roles([("bridge", 1), ("verifiers", 1), ("bridge", 2), ("governance", 3)]),
            BTreeSet::from(["bridge"])
        );
        assert!(duplicate_roles::<u8, u8>([]).is_empty());
    }

    #[test]
    fn prefix_uncertainty_cannot_admit_or_permanently_reject_a_rotation() {
        assert_eq!(admit_rotation(false, false), RotationAdmission::NotReady);
        assert_eq!(admit_rotation(false, true), RotationAdmission::NotReady);
        assert_eq!(admit_rotation(true, true), RotationAdmission::RejectedEarlierUnfinalizedProof);
        assert_eq!(admit_rotation(true, false), RotationAdmission::Accepted);
    }
}
