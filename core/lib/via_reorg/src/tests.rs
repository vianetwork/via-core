use std::{
    collections::HashMap,
    future::{ready, Future},
    num::NonZeroU32,
};

use bitcoin::BlockHash;
use via_test_utils::chain::Chain;

use crate::{chain::*, maturity::*, votes::tally};

fn headers(chain: &Chain) -> HashMap<BlockHash, Header<BlockHash>> {
    chain
        .blocks
        .iter()
        .map(|(&height, block)| {
            (
                block.block_hash(),
                Header {
                    height,
                    block_hash: block.block_hash(),
                    parent_hash: block.header.prev_blockhash,
                },
            )
        })
        .collect()
}

struct Source(HashMap<BlockHash, Header<BlockHash>>);
impl HeaderSource<BlockHash> for Source {
    type Error = ();
    fn header(
        &mut self,
        hash: BlockHash,
    ) -> impl Future<Output = Result<Option<Header<BlockHash>>, ()>> + Send {
        ready(Ok(self.0.get(&hash).copied()))
    }
}

async fn difference(
    old: &Chain,
    new: &Chain,
    depth: u32,
    frontier: u32,
) -> Result<ChainDifference<BlockHash>, WalkError<()>> {
    let retained = headers(old);
    let source = headers(new);
    let previous = retained[&old.blocks.values().next_back().unwrap().block_hash()];
    let height = previous.height.min(*new.blocks.keys().next_back().unwrap());
    find_difference(
        source[&new.blocks[&height].block_hash()],
        previous,
        old.blocks[&0].block_hash(),
        &mut |hash| retained.get(&hash).copied(),
        &mut Source(source),
        NonZeroU32::new(depth).unwrap(),
        retained[&old.blocks[&frontier].block_hash()].into(),
    )
    .await
}

// LDK equal/shorter/longer fork cases, adapted to the one shared Via chain fixture and recovery start.
// https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/lib.rs
#[tokio::test]
async fn fork_shapes_and_inclusive_old_side_limit() {
    for depth in [1, 6, 100] {
        for removed in [0, depth - 1, depth, depth + 1] {
            for added in [0, 2, depth + 2] {
                let old = Chain::with_height(removed + 3);
                let mut new = Chain::with_height(added + 3);
                new.fork_at_height(3);
                let result = difference(&old, &new, depth, 0).await;
                if removed >= depth {
                    assert_eq!(result, Err(WalkError::OldBranchLimit));
                } else {
                    let result = result.unwrap();
                    assert_eq!(result.common_ancestor.height, 3);
                    assert_eq!(
                        result.disconnected_blocks.iter().map(|h| h.height).collect::<Vec<_>>(),
                        (4..=removed + 3).rev().collect::<Vec<_>>()
                    );
                    assert_eq!(
                        result.connected_blocks.iter().map(|h| h.height).collect::<Vec<_>>(),
                        (4..=3 + added.min(removed)).collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn successive_shrinks_cannot_cross_the_confirmed_frontier() {
    for (depth, first, second, third, frontier) in
        [(100, 1000, 950, 900, 901), (6, 100, 97, 94, 95)]
    {
        assert_eq!(
            difference(&Chain::with_height(first), &Chain::with_height(second), depth, frontier)
                .await
                .unwrap()
                .common_ancestor
                .height,
            second
        );
        assert_eq!(
            difference(&Chain::with_height(second), &Chain::with_height(third), depth, frontier)
                .await,
            Err(WalkError::ConfirmedFrontier)
        );
    }
}

#[tokio::test]
async fn missing_retained_parent_is_integrity_failure() {
    let old = Chain::with_height(5);
    let mut new = Chain::with_height(5);
    new.fork_at_height(2);
    let retained = headers(&old);
    let source = headers(&new);
    assert_eq!(
        find_difference(
            source[&new.blocks[&5].block_hash()],
            retained[&old.blocks[&5].block_hash()],
            old.blocks[&0].block_hash(),
            &mut |_| None,
            &mut Source(source),
            NonZeroU32::new(6).unwrap(),
            retained[&old.blocks[&0].block_hash()].into(),
        )
        .await,
        Err(WalkError::MissingRetainedHeader)
    );
}

#[test]
fn maturity_includes_source_block_and_rejects_future_heights() {
    for depth in [1, 6, 100] {
        assert!(!is_l1_reorg_safe(101, 100, depth));
        assert!(is_l1_reorg_safe(100, 99 + depth, depth));
        if depth > 1 {
            assert!(!is_l1_reorg_safe(100, 98 + depth, depth));
        }
    }
    assert_eq!(compute_confirmation_depth(0, u32::MAX), Some(u32::MAX));
    assert!(is_l1_reorg_safe(100, 100, 0));
}

#[test]
fn exact_proof_and_historical_members_determine_the_tally() {
    let members = [1, 2, 3];
    let votes = [(10, 1, true), (10, 1, true), (11, 2, true), (10, 9, true)];
    assert_eq!(tally(&10, &members, votes, 0.66), None);
    assert_eq!(tally(&10, &members, votes.into_iter().chain([(10, 2, true)]), 0.66), Some(true));
    assert_eq!(tally(&10, &members, [(10, 1, false), (10, 2, false)], 0.66), Some(false));
    assert_eq!(tally(&10, &[1, 2], [(10, 1, true), (10, 2, false)], 0.5), Some(true));
}
