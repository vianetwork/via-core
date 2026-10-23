//! Exact-proof quorum with the verifier set active before that proof's inclusion.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vote<P, V, I> {
    pub identity: I,
    pub proof: P,
    pub verifier: V,
    pub approved: bool,
    /// Height, transaction index, and stable in-transaction message index.
    pub position: [u32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TallyError {
    MissingHistoricalSet,
    ConflictingEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub approvals: usize,
    pub rejections: usize,
    /// Approval wins a tie, preserving Via's existing threshold interpretation.
    pub finalized: Option<bool>,
}

/// Callers supply canonical votes and the historical set; a startup/latest set is not a substitute.
/// Adapted eligibility / unique-signer checks from CometBFT, pinned at
/// <https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/validation.go#L65-L138>.
/// Via retains its winning-side ratio rather than importing CometBFT's quorum formula.
pub fn tally<P: Eq, V: Ord, I: Ord>(
    proof: &P,
    historical_set: Option<&[V]>,
    votes: &[Vote<P, V, I>],
    threshold: f64,
) -> Result<Tally, TallyError> {
    let members = historical_set.ok_or(TallyError::MissingHistoricalSet)?;
    let eligible: BTreeSet<_> = members.iter().collect();
    let mut evidence = BTreeMap::new();
    for vote in votes {
        if let Some(previous) = evidence.insert(&vote.identity, vote) {
            if previous != vote {
                return Err(TallyError::ConflictingEvidence);
            }
        }
    }
    let mut ordered: Vec<_> = evidence.into_values().filter(|vote| &vote.proof == proof).collect();
    ordered.sort_by_key(|vote| (vote.position, &vote.identity));
    let mut seen = BTreeSet::new();
    let mut approvals = 0;
    let mut rejections = 0;
    for vote in ordered {
        if eligible.contains(&vote.verifier) && seen.insert(&vote.verifier) {
            if vote.approved {
                approvals += 1;
            } else {
                rejections += 1;
            }
        }
    }
    let approved = approvals >= rejections;
    let count = if approved { approvals } else { rejections };
    let finalized = (count as f64 / members.len() as f64 >= threshold).then_some(approved);
    Ok(Tally { approvals, rejections, finalized })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vote(
        identity: u8,
        proof: u8,
        verifier: u8,
        approved: bool,
        height: u32,
    ) -> Vote<u8, u8, u8> {
        Vote { identity, proof, verifier, approved, position: [height, 0, 0] }
    }

    #[test]
    fn two_proofs_of_one_batch_never_pool_even_the_same_verifier() {
        let votes = [vote(1, 10, 1, true, 1), vote(2, 11, 1, true, 2), vote(3, 11, 2, true, 3)];
        assert_eq!(tally(&10, Some(&[1, 2, 3]), &votes, 0.66).unwrap().finalized, None);
        assert_eq!(tally(&11, Some(&[1, 2, 3]), &votes, 0.66).unwrap().finalized, Some(true));
    }

    #[test]
    fn only_first_canonical_vote_of_each_historical_member_counts() {
        let votes = [
            vote(1, 10, 1, true, 9),
            vote(2, 10, 1, false, 8),
            vote(3, 10, 2, false, 9),
            vote(4, 10, 9, true, 7),
        ];
        assert_eq!(
            tally(&10, Some(&[1, 2, 3]), &votes, 0.66),
            Ok(Tally { approvals: 0, rejections: 2, finalized: Some(false) })
        );
        let mut reordered = votes;
        reordered.reverse();
        assert_eq!(tally(&10, Some(&[1, 2, 3]), &reordered, 0.66).unwrap().rejections, 2);
    }

    #[test]
    fn threshold_and_tie_preserve_existing_policy() {
        let votes = [vote(1, 10, 1, true, 1), vote(2, 10, 2, false, 1)];
        assert_eq!(tally(&10, Some(&[1, 2]), &votes, 0.5).unwrap().finalized, Some(true));
        assert_eq!(tally(&10, Some(&[1, 2]), &votes, 0.51).unwrap().finalized, None);
        assert_eq!(tally(&10, Some(&[1, 2, 2]), &votes, 0.5).unwrap().finalized, None);
        assert_eq!(tally::<u8, u8, u8>(&10, Some(&[]), &[], 0.5).unwrap().finalized, None);
    }

    #[test]
    fn historical_set_is_required_even_without_votes() {
        assert_eq!(tally::<u8, u8, u8>(&1, None, &[], 0.5), Err(TallyError::MissingHistoricalSet));
    }

    #[test]
    fn exact_replay_is_one_vote_but_identity_conflicts_are_errors() {
        let original = vote(1, 10, 1, true, 1);
        assert_eq!(
            tally(&10, Some(&[1]), &[original.clone(), original.clone()], 1.0).unwrap().approvals,
            1
        );
        for conflict in [
            Vote { proof: 11, ..original.clone() },
            Vote { verifier: 2, ..original.clone() },
            Vote { approved: false, ..original.clone() },
            Vote { position: [2, 0, 0], ..original.clone() },
        ] {
            assert_eq!(
                tally(&10, Some(&[1]), &[original.clone(), conflict], 1.0),
                Err(TallyError::ConflictingEvidence)
            );
        }
    }
}
