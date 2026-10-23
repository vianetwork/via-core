//! A complete, checked difference is prepared before any store is changed.

use std::{future::Future, num::NonZeroUsize, ops::Deref};

pub const MAX_REORG_DEPTH: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRef<H> {
    pub block_hash: H,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header<H> {
    pub block_hash: H,
    pub parent_hash: H,
    pub height: u32,
}

impl<H> From<Header<H>> for BlockRef<H> {
    fn from(header: Header<H>) -> Self {
        Self { block_hash: header.block_hash, height: header.height }
    }
}

/// Only the candidate branch uses the source. Retained ancestry never falls back to today's chain.
pub trait HeaderSource<H> {
    type Error;
    fn header(
        &mut self,
        hash: H,
    ) -> impl Future<Output = Result<Option<Header<H>>, Self::Error>> + Send;
}

#[derive(Debug, PartialEq, Eq)]
pub enum WalkError<E> {
    MissingRetainedHeader,
    UnknownSourceHeader,
    Source(E),
    WrongHash,
    WrongHeight,
    WrongGenesis,
    OldBranchLimit,
    PreparationLimit,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ChainDifference<H> {
    pub common_ancestor: Header<H>,
    /// Undo in this order, then apply `connected_blocks` in its forward order.
    pub disconnected_blocks: Vec<Header<H>>,
    pub connected_blocks: Vec<Header<H>>,
}

#[derive(Clone, Copy)]
struct WalkHeader<H> {
    header: Header<H>,
    retained: bool,
}

impl<H> Deref for WalkHeader<H> {
    type Target = Header<H>;
    fn deref(&self) -> &Self::Target {
        &self.header
    }
}

struct Walk<'a, H, R> {
    retained: &'a mut R,
    genesis: H,
    disconnected: Vec<Header<H>>,
    connected: usize,
    preparation_limit: NonZeroUsize,
}

impl<H: Copy + Eq, R: FnMut(H) -> Option<Header<H>>> Walk<'_, H, R> {
    async fn look_up_previous_header<P: HeaderSource<H>>(
        &mut self,
        chain_poller: &mut P,
        header: &WalkHeader<H>,
    ) -> Result<WalkHeader<H>, WalkError<P::Error>> {
        let height = header.height.checked_sub(1).ok_or(WalkError::WrongGenesis)?;
        let parent = if header.retained {
            if self.disconnected.len() >= MAX_REORG_DEPTH {
                return Err(WalkError::OldBranchLimit);
            }
            let parent =
                (self.retained)(header.parent_hash).ok_or(WalkError::MissingRetainedHeader)?;
            self.disconnected.push(header.header);
            parent
        } else {
            if self.connected >= self.preparation_limit.get() {
                return Err(WalkError::PreparationLimit);
            }
            self.connected += 1;
            chain_poller
                .header(header.parent_hash)
                .await
                .map_err(WalkError::Source)?
                .ok_or(WalkError::UnknownSourceHeader)?
        };
        if parent.block_hash != header.parent_hash {
            return Err(WalkError::WrongHash);
        }
        if parent.height != height {
            return Err(WalkError::WrongHeight);
        }
        if height == 0 && parent.block_hash != self.genesis {
            return Err(WalkError::WrongGenesis);
        }
        Ok(WalkHeader { header: parent, retained: header.retained })
    }

    // The traversal loop is retained byte-for-byte. SQL ancestry, errors and budgets are Via adapters.
    // https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/lib.rs
    #[rustfmt::skip]
    async fn find_difference_from_header<P: HeaderSource<H>>(
        &mut self, current_header: WalkHeader<H>, prev_header: &WalkHeader<H>, chain_poller: &mut P,
    ) -> Result<ChainDifference<H>, WalkError<P::Error>> {
        let mut connected_blocks = Vec::new();
        let mut current = current_header;
        let mut previous = *prev_header;
        // provenance:ldk-walk:start
		loop {
			// Found the common ancestor.
			if current.block_hash == previous.block_hash {
				break;
			}

			// Walk back the chain, finding blocks needed to connect and disconnect. Only walk back
			// the header with the greater height, or both if equal heights.
			let current_height = current.height;
			let previous_height = previous.height;
			if current_height <= previous_height {
				previous = self.look_up_previous_header(chain_poller, &previous).await?;
			}
			if current_height >= previous_height {
				connected_blocks.push(current);
				current = self.look_up_previous_header(chain_poller, &current).await?;
			}
		}
        // provenance:ldk-walk:end
        if current.header != previous.header {
            return Err(WalkError::WrongHeight);
        }
        connected_blocks.reverse();
        Ok(ChainDifference {
            common_ancestor: current.header,
            disconnected_blocks: std::mem::take(&mut self.disconnected),
            connected_blocks: connected_blocks.into_iter().map(|header| header.header).collect(),
        })
    }
}

/// `preparation_limit` bounds candidate memory; catch-up chunks do not consume the old-side budget.
pub async fn find_difference<H: Copy + Eq, P: HeaderSource<H>>(
    current: Header<H>,
    previous: Header<H>,
    genesis: H,
    retained: &mut impl FnMut(H) -> Option<Header<H>>,
    source: &mut P,
    preparation_limit: NonZeroUsize,
) -> Result<ChainDifference<H>, WalkError<P::Error>> {
    if [current, previous].iter().any(|header| header.height == 0 && header.block_hash != genesis) {
        return Err(WalkError::WrongGenesis);
    }
    if current.block_hash == previous.block_hash && current != previous {
        return Err(WalkError::WrongHeight);
    }
    Walk { retained, genesis, disconnected: Vec::new(), connected: 0, preparation_limit }
        .find_difference_from_header(
            WalkHeader { header: current, retained: false },
            &WalkHeader { header: previous, retained: true },
            source,
        )
        .await
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        future::ready,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
    };

    use super::*;

    struct Source(BTreeMap<u32, Header<u32>>);
    impl HeaderSource<u32> for Source {
        type Error = &'static str;
        fn header(
            &mut self,
            hash: u32,
        ) -> impl Future<Output = Result<Option<Header<u32>>, Self::Error>> + Send {
            ready(Ok(self.0.get(&hash).copied()))
        }
    }

    fn run<F: Future>(future: F) -> F::Output {
        struct NoWake;
        impl Wake for NoWake {
            fn wake(self: Arc<Self>) {}
        }
        let waker = Waker::from(Arc::new(NoWake));
        match std::pin::pin!(future).as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("fixture lookups complete immediately"),
        }
    }

    fn chain(tip: u32, fork: u32, offset: u32) -> BTreeMap<u32, Header<u32>> {
        let mut headers = BTreeMap::new();
        let mut parent_hash = 0;
        for height in 0..=tip {
            let block_hash = height + if height > fork { offset } else { 0 };
            headers.insert(block_hash, Header { block_hash, parent_hash, height });
            parent_hash = block_hash;
        }
        headers
    }

    fn difference(
        old_tip: u32,
        new_tip: u32,
        fork: u32,
    ) -> Result<ChainDifference<u32>, WalkError<&'static str>> {
        let old = chain(old_tip, old_tip, 0);
        let new = chain(new_tip, fork, 1000);
        run(find_difference(
            *new.values().max_by_key(|header| header.height).unwrap(),
            old[&old_tip],
            0,
            &mut |hash| old.get(&hash).copied(),
            &mut Source(new),
            NonZeroUsize::new(2048).unwrap(),
        ))
    }

    // Port the LDK same/different, equal/shorter/longer, missing/no-new/some-new cases.
    #[test]
    fn fork_shapes_and_transition_order() {
        for (old, new, fork) in
            [(5, 5, 5), (5, 5, 0), (5, 5, 2), (5, 3, 2), (5, 8, 2), (5, 5, 5), (5, 8, 5)]
        {
            let diff = difference(old, new, fork).unwrap();
            assert_eq!(diff.common_ancestor.height, fork.min(old).min(new));
            assert_eq!(
                diff.disconnected_blocks.iter().map(|header| header.height).collect::<Vec<_>>(),
                ((fork + 1)..=old).rev().collect::<Vec<_>>()
            );
            assert_eq!(
                diff.connected_blocks.iter().map(|header| header.height).collect::<Vec<_>>(),
                ((fork + 1)..=new).collect::<Vec<_>>()
            );
            assert!(diff.connected_blocks.iter().all(|header| header.block_hash
                == header.height + if header.height > fork { 1000 } else { 0 }));
        }
    }

    #[test]
    fn old_depth_is_inclusive_and_does_not_charge_replacement_catchup() {
        assert_eq!(difference(101, 1001, 1).unwrap().disconnected_blocks.len(), 100);
        assert_eq!(difference(102, 103, 1), Err(WalkError::OldBranchLimit));
        assert!(difference(1, 1500, 1).unwrap().disconnected_blocks.is_empty());
    }

    #[test]
    fn missing_retained_history_is_not_replaced_with_source_history() {
        let old = chain(5, 5, 0);
        let new = chain(5, 2, 1000);
        assert_eq!(
            run(find_difference(
                new[&1005],
                old[&5],
                0,
                &mut |_| None,
                &mut Source(new),
                NonZeroUsize::new(50).unwrap()
            )),
            Err(WalkError::MissingRetainedHeader)
        );
    }

    #[test]
    fn missing_source_and_malformed_parent_refuse_preparation() {
        let old = chain(1, 1, 0);
        let mut new = chain(3, 1, 1000);
        let tip = new[&1003];
        for (parent, expected) in [
            (None, WalkError::UnknownSourceHeader),
            (Some(Header { block_hash: 1001, parent_hash: 1, height: 2 }), WalkError::WrongHash),
            (Some(Header { block_hash: 1002, parent_hash: 1, height: 1 }), WalkError::WrongHeight),
        ] {
            new.remove(&1002);
            if let Some(parent) = parent {
                new.insert(1002, parent);
            }
            assert_eq!(
                run(find_difference(
                    tip,
                    old[&1],
                    0,
                    &mut |hash| old.get(&hash).copied(),
                    &mut Source(new.clone()),
                    NonZeroUsize::new(50).unwrap()
                )),
                Err(expected)
            );
        }
    }

    #[test]
    fn wrong_genesis_and_underflow_refuse_preparation() {
        let bad = Header { block_hash: 10, parent_hash: 0, height: 0 };
        assert_eq!(
            run(find_difference(
                bad,
                Header { block_hash: 0, ..bad },
                0,
                &mut |_| None,
                &mut Source(BTreeMap::new()),
                NonZeroUsize::new(1).unwrap()
            )),
            Err(WalkError::WrongGenesis)
        );
        assert_eq!(difference(1, 0, 0).unwrap().disconnected_blocks.len(), 1);
    }

    #[test]
    fn unavailable_source_and_inconsistent_common_identity_refuse_preparation() {
        struct Unavailable;
        impl HeaderSource<u32> for Unavailable {
            type Error = &'static str;
            fn header(
                &mut self,
                _: u32,
            ) -> impl Future<Output = Result<Option<Header<u32>>, Self::Error>> + Send {
                ready(Err("transport unavailable"))
            }
        }
        let old = chain(3, 3, 0);
        let tip = Header { block_hash: 1005, parent_hash: 1004, height: 5 };
        assert_eq!(
            run(find_difference(
                tip,
                old[&3],
                0,
                &mut |hash| old.get(&hash).copied(),
                &mut Unavailable,
                NonZeroUsize::new(10).unwrap()
            )),
            Err(WalkError::Source("transport unavailable"))
        );
        let mut new = BTreeMap::new();
        new.insert(1004, Header { block_hash: 1004, parent_hash: 3, height: 4 });
        new.insert(3, Header { block_hash: 3, parent_hash: 999, height: 3 });
        assert_eq!(
            run(find_difference(
                tip,
                old[&3],
                0,
                &mut |hash| old.get(&hash).copied(),
                &mut Source(new),
                NonZeroUsize::new(10).unwrap()
            )),
            Err(WalkError::WrongHeight)
        );
        assert_eq!(
            run(find_difference(
                Header { parent_hash: 999, ..old[&3] },
                old[&3],
                0,
                &mut |hash| old.get(&hash).copied(),
                &mut Source(old.clone()),
                NonZeroUsize::new(10).unwrap()
            )),
            Err(WalkError::WrongHeight)
        );
    }

    #[test]
    fn candidate_preparation_is_bounded() {
        let old = chain(1, 1, 0);
        let new = chain(5, 1, 1000);
        assert_eq!(
            run(find_difference(
                new[&1005],
                old[&1],
                0,
                &mut |hash| old.get(&hash).copied(),
                &mut Source(new),
                NonZeroUsize::new(3).unwrap()
            )),
            Err(WalkError::PreparationLimit)
        );
    }
}
