//! Node loops schedule these decisions; only committed hash positions determine catch-up.

use std::num::NonZeroU32;

use crate::chain::Header;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanGroup {
    Configuration,
    Deposits,
    ProofsAndVotes,
    Withdrawals,
}

impl ScanGroup {
    pub const ALL: [Self; 4] =
        [Self::Configuration, Self::Deposits, Self::ProofsAndVotes, Self::Withdrawals];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Deposits => "deposits",
            Self::ProofsAndVotes => "proofs_and_votes",
            Self::Withdrawals => "withdrawals",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    Canonical(NonZeroU32),
    Noncanonical,
    Unknown,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceProblem {
    UnknownMembership,
    Unavailable,
    InconsistentTip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep<H> {
    Reconcile { previous: Header<H>, candidate: Header<H> },
    CatchUp { after: Header<H>, through: u32, observed_tip: Header<H> },
    ReconcilePending,
    Idle,
    Hold(SourceProblem),
}

/// Like Core's BaseIndex, restart resumes from committed chain identity and reconciles before advancing.
/// https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/index/base.cpp
/// Membership is a fresh answer about the stored hash, never an inference from tip height.
/// Pending committed work is reconciled even when the source has not produced another block.
pub fn next_step<H: Copy + Eq>(
    position: Header<H>,
    observed_tip: Header<H>,
    membership: Membership,
    pending_work: bool,
    chunk_size: NonZeroU32,
) -> NextStep<H> {
    match membership {
        Membership::Unknown => return NextStep::Hold(SourceProblem::UnknownMembership),
        Membership::Unavailable => return NextStep::Hold(SourceProblem::Unavailable),
        Membership::Noncanonical => {
            return NextStep::Reconcile { previous: position, candidate: observed_tip }
        }
        Membership::Canonical(_) => {}
    }
    if observed_tip.height < position.height
        || (observed_tip.height == position.height && observed_tip != position)
        || (observed_tip.block_hash == position.block_hash && observed_tip != position)
    {
        return NextStep::Hold(SourceProblem::InconsistentTip);
    }
    if pending_work {
        return NextStep::ReconcilePending;
    }
    if observed_tip == position {
        return NextStep::Idle;
    }
    NextStep::CatchUp {
        after: position,
        through: position.height.saturating_add(chunk_size.get()).min(observed_tip.height),
        observed_tip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_names_are_the_complete_persisted_contract() {
        assert_eq!(
            ScanGroup::ALL.map(ScanGroup::as_str),
            ["configuration", "deposits", "proofs_and_votes", "withdrawals"]
        );
    }

    fn header(height: u32, block_hash: u32) -> Header<u32> {
        Header { height, block_hash, parent_hash: block_hash.saturating_sub(1) }
    }

    fn step(
        position: Header<u32>,
        tip: Header<u32>,
        membership: Membership,
        pending: bool,
    ) -> NextStep<u32> {
        next_step(position, tip, membership, pending, NonZeroU32::new(50).unwrap())
    }

    #[test]
    fn authoritative_disconnection_requires_no_new_block() {
        let position = header(10, 10);
        for tip in [header(9, 9), header(10, 20), header(11, 21)] {
            assert_eq!(
                step(position, tip, Membership::Noncanonical, false),
                NextStep::Reconcile { previous: position, candidate: tip }
            );
        }
    }

    #[test]
    fn source_uncertainty_never_authorizes_undo_or_pending_actions() {
        for pending in [false, true] {
            assert_eq!(
                step(header(10, 10), header(9, 9), Membership::Unknown, pending),
                NextStep::Hold(SourceProblem::UnknownMembership)
            );
            assert_eq!(
                step(header(10, 10), header(9, 9), Membership::Unavailable, pending),
                NextStep::Hold(SourceProblem::Unavailable)
            );
        }
        for tip in [header(9, 9), header(10, 20), header(11, 10)] {
            assert_eq!(
                step(
                    header(10, 10),
                    tip,
                    Membership::Canonical(NonZeroU32::new(1).unwrap()),
                    false
                ),
                NextStep::Hold(SourceProblem::InconsistentTip)
            );
        }
    }

    #[test]
    fn restart_above_agreement_catches_up_from_the_committed_agreement() {
        // BDK #2167: the pre-reorg position must not become the replacement start.
        let canonical = Membership::Canonical(NonZeroU32::new(1).unwrap());
        for (position, tip, through) in [
            (header(4, 4), header(100, 1100), 54),
            (header(54, 1054), header(100, 1100), 100),
            (header(u32::MAX - 1, 10), header(u32::MAX, u32::MAX), u32::MAX),
        ] {
            assert_eq!(
                step(position, tip, canonical, false),
                NextStep::CatchUp { after: position, through, observed_tip: tip }
            );
        }
    }

    #[test]
    fn restart_materializes_committed_work_before_another_block_arrives() {
        // Alpen #1944: canonical SQL may commit before its dependent materialization.
        let tip = header(10, 10);
        let canonical = Membership::Canonical(NonZeroU32::new(1).unwrap());
        assert_eq!(step(tip, tip, canonical, true), NextStep::ReconcilePending);
        assert_eq!(step(tip, tip, canonical, false), NextStep::Idle);
    }
}
