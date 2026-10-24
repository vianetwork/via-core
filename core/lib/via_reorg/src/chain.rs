//! A complete, checked difference is prepared before any store is changed.

use std::{future::Future, num::NonZeroU32, ops::Deref};

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
    ConfirmedFrontier,
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
    depth: NonZeroU32,
    frontier: BlockRef<H>,
}

impl<H: Copy + Eq, R: FnMut(H) -> Option<Header<H>>> Walk<'_, H, R> {
    async fn look_up_previous_header<P: HeaderSource<H>>(
        &mut self,
        chain_poller: &mut P,
        header: &WalkHeader<H>,
    ) -> Result<WalkHeader<H>, WalkError<P::Error>> {
        let height = header.height.checked_sub(1).ok_or(WalkError::WrongGenesis)?;
        let parent = if header.retained {
            if self.disconnected.len() + 1 >= self.depth.get() as usize {
                return Err(WalkError::OldBranchLimit);
            }
            if height < self.frontier.height {
                return Err(WalkError::ConfirmedFrontier);
            }
            let parent =
                (self.retained)(header.parent_hash).ok_or(WalkError::MissingRetainedHeader)?;
            self.disconnected.push(header.header);
            parent
        } else {
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
        if header.retained
            && height == self.frontier.height
            && parent.block_hash != self.frontier.block_hash
        {
            return Err(WalkError::ConfirmedFrontier);
        }
        Ok(WalkHeader { header: parent, retained: header.retained })
    }

    // LDK find_difference_from_header: loop unchanged; checked retained ancestry and N/frontier
    // bounds live in the Via adapter. Source catch-up starts at min(old height, selected height).
    // https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/lib.rs
    #[rustfmt::skip]
    async fn find_difference_from_header<P: HeaderSource<H>>(
        &mut self, current_header: WalkHeader<H>, prev_header: &WalkHeader<H>, chain_poller: &mut P,
    ) -> Result<ChainDifference<H>, WalkError<P::Error>> {
        let mut connected_blocks = Vec::new();
        let mut current = current_header;
        let mut previous = *prev_header;
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

/// Prepare recovery from the selected canonical header at min(stored height, selected height).
/// Forward ingestion is separate; only disconnected old blocks consume the N-depth budget.
pub async fn find_difference<H: Copy + Eq, P: HeaderSource<H>>(
    current: Header<H>,
    previous: Header<H>,
    genesis: H,
    retained: &mut impl FnMut(H) -> Option<Header<H>>,
    source: &mut P,
    depth: NonZeroU32,
    frontier: BlockRef<H>,
) -> Result<ChainDifference<H>, WalkError<P::Error>> {
    if [current, previous].iter().any(|header| header.height == 0 && header.block_hash != genesis) {
        return Err(WalkError::WrongGenesis);
    }
    if current.block_hash == previous.block_hash && current != previous {
        return Err(WalkError::WrongHeight);
    }
    if previous.height < frontier.height
        || (previous.height == frontier.height && previous.block_hash != frontier.block_hash)
    {
        return Err(WalkError::ConfirmedFrontier);
    }
    Walk { retained, genesis, disconnected: Vec::new(), depth, frontier }
        .find_difference_from_header(
            WalkHeader { header: current, retained: false },
            &WalkHeader { header: previous, retained: true },
            source,
        )
        .await
}
