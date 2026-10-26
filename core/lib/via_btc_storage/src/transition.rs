//! RPC preparation is separate from the role-owned SQL transaction and suffix deletion.

use std::collections::HashMap;

use anyhow::{ensure, Context};
use bitcoin::{Block, BlockHash};
use via_btc_client::{
    client::BitcoinClient,
    traits::BitcoinOps,
    types::{BitcoinError, BlockHeaderInfo, HeaderMembership},
};
use via_reorg::chain::{find_difference, BlockRef, Header, HeaderSource, WalkError};
use zksync_db_connection::instrument::InstrumentExt;

use crate::{ReaderDal, ReaderState, StoreError, StoredHeader};

pub struct ReaderSnapshot {
    pub state: ReaderState,
    pub head: StoredHeader,
    retained: HashMap<BlockHash, StoredHeader>,
}

impl ReaderSnapshot {
    pub async fn load<DB: zksync_db_connection::connection::DbMarker>(
        reader: &mut ReaderDal<'_, '_, DB>,
        depth: std::num::NonZeroU32,
    ) -> anyhow::Result<Self> {
        let state = reader.state().await?.context("Bitcoin reader is not initialized")?;
        let head = reader.get_highest_block().await?.context("Bitcoin reader has no head")?;
        let first = head.header.height.saturating_sub(depth.get() - 1).max(state.confirmed.height);
        let rows = sqlx::query_as::<_, crate::HeaderRow>(
            "SELECT id,number,hash,parent_hash FROM via_btc_headers WHERE number>=$1 AND number<=$2",
        ).bind(i64::from(first)).bind(i64::from(head.header.height))
            .instrument("bitcoin_reader_retained_headers").fetch_all(reader.storage).await?;
        let retained = rows
            .into_iter()
            .map(|row| {
                let stored = StoredHeader::try_from(row)?;
                Ok((stored.header.block_hash, stored))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(Self { state, head, retained })
    }
}

pub enum Transition {
    Idle,
    Append { header: Header<BlockHash>, body: Block },
    Rewind { ancestor: StoredHeader },
    Stop(String),
}

/// Existing kept facts whose full canonical binding has not yet been replayed.
pub struct RetainedProofs(Vec<Option<BlockHeaderInfo>>);

impl RetainedProofs {
    pub async fn prepare(client: &BitcoinClient, txids: &[bitcoin::Txid]) -> anyhow::Result<Self> {
        let mut origins = Vec::with_capacity(txids.len());
        for txid in txids {
            origins.push(client.canonical_transaction(txid).await?.map(|(_, header, _)| header));
        }
        Ok(Self(origins))
    }

    pub fn complete_through(&self, height: u32) -> bool {
        self.0.iter().all(|origin| origin.is_some_and(|origin| origin.height > height))
    }

    pub async fn revalidate(&self, client: &BitcoinClient) -> anyhow::Result<()> {
        for origin in self.0.iter().flatten() {
            ensure!(
                client.canonical_header_at(origin.height).await?.hash == origin.hash,
                "retained proof source changed"
            );
        }
        Ok(())
    }
}

pub struct PreparedTransition {
    pub snapshot: ReaderSnapshot,
    pub selected: BlockRef<BlockHash>,
    pub transition: Transition,
    confirmed: Option<Header<BlockHash>>,
    observed_head: BlockHeaderInfo,
    pub(crate) depth: std::num::NonZeroU32,
}

fn header(value: BlockHeaderInfo) -> Header<BlockHash> {
    Header { height: value.height, block_hash: value.hash, parent_hash: value.parent_hash }
}

struct Source<'a>(&'a dyn BitcoinOps);
impl HeaderSource<BlockHash> for Source<'_> {
    type Error = BitcoinError;
    async fn header(&mut self, hash: BlockHash) -> Result<Option<Header<BlockHash>>, Self::Error> {
        self.0.header_by_hash(&hash).await.map(|value| value.map(header))
    }
}

impl PreparedTransition {
    /// Missing/lagging/unavailable source evidence returns an error and authorizes no writes.
    /// Only Core's explicit negative confirmation proves disconnection (BDK LocalChain update).
    /// https://github.com/bitcoindevkit/bdk/blob/5dc6e959379982a1661e8f57c3ee7af334627ca5/crates/bitcoind_rpc/src/lib.rs
    pub async fn prepare(
        client: &dyn BitcoinOps,
        snapshot: ReaderSnapshot,
    ) -> anyhow::Result<Self> {
        let selected_hash = client.best_block_hash().await?;
        let selected = client
            .header_by_hash(&selected_hash)
            .await?
            .context(StoreError::SourceHeld("unknown selected tip"))?;
        ensure!(
            matches!(selected.membership, HeaderMembership::Canonical(_)),
            StoreError::SourceHeld("selected tip changed")
        );
        let old = client
            .header_by_hash(&snapshot.head.header.block_hash)
            .await?
            .context(StoreError::SourceHeld("Core does not know retained head; authority held"))?;
        ensure!(
            header(old) == snapshot.head.header,
            StoreError::Integrity("retained head differs from source header")
        );
        let depth = client.reorg_safe_depth()?;
        let transition = if let Some(reason) = &snapshot.state.stopped_reason {
            Transition::Stop(reason.clone())
        } else if old.membership == HeaderMembership::Noncanonical {
            let current = client.canonical_header_at(old.height.min(selected.height)).await?;
            let result = find_difference(
                header(current),
                snapshot.head.header,
                snapshot.state.genesis,
                &mut |hash| snapshot.retained.get(&hash).map(|stored| stored.header),
                &mut Source(client),
                depth,
                snapshot.state.confirmed,
            )
            .await;
            match result {
                Ok(difference) => Transition::Rewind {
                    ancestor: snapshot.retained.get(&difference.common_ancestor.block_hash)
                        .context("proved ancestor is absent from retained history")?.clone(),
                },
                Err(WalkError::Source(error)) => return Err(error.into()),
                Err(WalkError::UnknownSourceHeader) => anyhow::bail!(StoreError::SourceHeld("source ancestry unavailable; authority held")),
                Err(reason) => Transition::Stop(format!(
                    "stopped for diagnosis: network={} N={} old={:?} selected={:?} frontier={:?} reason={reason:?}",
                    snapshot.state.network, depth, snapshot.head.header, header(selected), snapshot.state.confirmed,
                )),
            }
        } else {
            ensure!(
                selected.height >= old.height,
                StoreError::SourceHeld("source is lagging; authority held")
            );
            if selected.height == old.height {
                ensure!(
                    selected.hash == old.hash,
                    StoreError::SourceHeld("source selection changed")
                );
                Transition::Idle
            } else {
                let next = client.canonical_header_at(old.height + 1).await?;
                ensure!(
                    next.parent_hash == old.hash,
                    StoreError::SourceHeld("source parent changed")
                );
                Transition::Append {
                    header: header(next),
                    body: client.checked_block(&next).await?,
                }
            }
        };
        let committed_height = match &transition {
            Transition::Append { header, .. } => header.height,
            Transition::Rewind { ancestor } => ancestor.header.height,
            _ => snapshot.head.header.height,
        };
        let confirmed = if matches!(transition, Transition::Stop(_)) {
            None
        } else {
            let protected = client.canonical_header_at(snapshot.state.confirmed.height).await?;
            ensure!(
                protected.hash == snapshot.state.confirmed.block_hash,
                StoreError::SourceHeld("confirmed source changed; authority held")
            );
            match selected
                .height
                .checked_sub(depth.get() - 1)
                .map(|height| height.min(committed_height))
            {
                Some(height) if height >= snapshot.state.confirmed.height => {
                    Some(header(client.canonical_header_at(height).await?))
                }
                _ => None,
            }
        };
        let prepared = Self {
            snapshot,
            selected: header(selected).into(),
            transition,
            confirmed,
            observed_head: old,
            depth,
        };
        prepared.revalidate(client).await?;
        Ok(prepared)
    }

    /// Invoke after all other RPC preparation, immediately before opening the outer transaction.
    pub async fn revalidate(&self, client: &dyn BitcoinOps) -> anyhow::Result<()> {
        ensure!(
            client.header_by_hash(&self.observed_head.hash).await? == Some(self.observed_head),
            StoreError::SourceHeld("source membership changed")
        );
        if let Transition::Append { header, .. } = &self.transition {
            ensure!(
                client.canonical_header_at(header.height).await?.hash == header.block_hash,
                StoreError::SourceHeld("prepared block changed")
            );
        }
        ensure!(
            client.best_block_hash().await? == self.selected.block_hash,
            StoreError::SourceHeld("source selection changed")
        );
        Ok(())
    }

    /// require_authority admits irreversible action only inside transaction with replay ready, not stopped,
    /// same frontier and head, and every dependency header mature and unchanged.
    /// Stale snapshot fails here instead of signing or voting on replaced history, so caller revalidates source first and holds owner lock through the action.
    /// Idea only from etcd concurrency Mutex IsOwner, which compares lock revision inside protected transaction: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/client/v3/concurrency/mutex.go
    pub async fn require_authority<DB: crate::ReaderRole>(
        &self,
        reader: &mut ReaderDal<'_, '_, DB>,
        dependencies: &[StoredHeader],
    ) -> anyhow::Result<()> {
        ensure!(
            reader.storage.in_transaction(),
            StoreError::Integrity("authority outside transaction")
        );
        ensure!(
            self.snapshot.state.replay_ready
                && self.snapshot.state.stopped_reason.is_none()
                && matches!(self.transition, Transition::Idle | Transition::Append { .. }),
            StoreError::AuthorityHeld
        );
        let state = reader.state().await?.context("reader state disappeared")?;
        ensure!(
            state.replay_ready
                && state.stopped_reason.is_none()
                && state.confirmed == self.snapshot.state.confirmed
                && reader.get_highest_block().await?.as_ref() == Some(&self.snapshot.head),
            StoreError::HeadChanged
        );
        ensure!(
            via_reorg::maturity::is_l1_reorg_safe(
                state.confirmed.height,
                self.selected.height,
                self.depth.get()
            ),
            StoreError::AuthorityHeld
        );
        let mature = self
            .selected
            .height
            .checked_sub(self.depth.get() - 1)
            .context(StoreError::AuthorityHeld)?;
        ensure!(state.confirmed.height >= mature, StoreError::AuthorityHeld);
        let complete = sqlx::query_scalar::<_, bool>("SELECT NOT EXISTS(SELECT 1 FROM unnest($1::TEXT[]) required(name)
            WHERE NOT EXISTS(SELECT 1 FROM via_btc_consumers c JOIN via_btc_headers h ON h.id=c.header_id
                WHERE c.name=required.name AND h.number>=$2))
            AND NOT EXISTS(SELECT 1 FROM via_btc_configuration WHERE l1_block_number<=$2 AND accepted IS NULL)")
            .bind(DB::REQUIRED_CONSUMERS).bind(i64::from(state.confirmed.height))
            .instrument("bitcoin_authority_consumer_prefix").fetch_one(reader.storage).await?;
        ensure!(complete, StoreError::AuthorityHeld);
        for dependency in dependencies {
            ensure!(
                dependency.header.height <= state.confirmed.height
                    && via_reorg::maturity::is_l1_reorg_safe(
                        dependency.header.height,
                        self.selected.height,
                        self.depth.get()
                    ),
                StoreError::AuthorityHeld
            );
            ensure!(
                reader.header_at(dependency.header.height).await?.as_ref() == Some(dependency),
                StoreError::HeadChanged
            );
        }
        Ok(())
    }

    /// The role deletes its dependent suffix first, in this same owner/source-locked transaction.
    /// A failed expected-head/frontier check rolls all those deletes back with the transaction.
    pub async fn apply_headers<DB: zksync_db_connection::connection::DbMarker>(
        &self,
        reader: &mut ReaderDal<'_, '_, DB>,
    ) -> anyhow::Result<()> {
        ensure!(
            reader.get_highest_block().await?.as_ref() == Some(&self.snapshot.head),
            StoreError::HeadChanged
        );
        let state = reader.state().await?.context("reader state disappeared")?;
        ensure!(
            state.confirmed == self.snapshot.state.confirmed
                && state.stopped_reason == self.snapshot.state.stopped_reason,
            StoreError::HeadChanged
        );
        match &self.transition {
            Transition::Append { header, .. } => {
                reader.save_block(*header, &self.snapshot.head).await?;
            }
            Transition::Rewind { ancestor } => {
                reader.rewind_headers(&self.snapshot.head, ancestor, self.selected).await?
            }
            Transition::Stop(reason) => reader.stop(reason).await?,
            Transition::Idle => {}
        }
        if let Some(confirmed) = self.confirmed {
            let boundary =
                reader.header_at(confirmed.height).await?.context("missing frontier header")?;
            ensure!(boundary.header == confirmed, StoreError::Integrity("frontier header changed"));
            let head = reader.get_highest_block().await?.context("head disappeared")?;
            reader.advance_frontier(&head, &boundary).await?;
        }
        Ok(())
    }
}

/// process_next prepares source reads, then commits one transition and its available consumer effects.
/// Native role writes and consumer progress share the transaction. Held interpretation never stops header observation.
/// Adapted from Sui's sequential committer, Bitcoin source rechecks and native suffix deletion differ because selected history can rewind: https://github.com/MystenLabs/sui/blob/6de525b6b70277c845f4f5751a9e87fc7130dd48/crates/sui-indexer-alt-framework/src/pipeline/sequential/committer.rs
pub async fn process_next<DB: crate::ReaderRole>(
    storage: &mut zksync_db_connection::connection::Connection<'_, DB>,
    client: &BitcoinClient,
    indexer: &mut via_btc_client::indexer::BitcoinInscriptionIndexer,
    threshold: f64,
) -> anyhow::Result<bool> {
    let result: anyhow::Result<bool> = async {
        let snapshot =
            ReaderSnapshot::load(&mut ReaderDal::new(storage), client.reorg_safe_depth()?).await?;
        let prepared = PreparedTransition::prepare(client, snapshot).await?;
        let mut block = None;
        let cursor = ReaderDal::new(storage)
            .consumer(crate::PROTOCOL_CONSUMER)
            .await?
            .context("protocol consumer is missing")?;
        if matches!(prepared.transition, Transition::Idle | Transition::Append { .. }) {
            let next =
                cursor.applied.header.height.checked_add(1).context("Bitcoin height overflow")?;
            let mut target = ReaderDal::new(storage).header_at(next).await?;
            let mut body = None;
            if let Transition::Append { header, body: checked } = &prepared.transition {
                if header.height == next {
                    target = Some(StoredHeader { id: 0, header: *header });
                    body = Some(checked);
                }
            }
            if let (Some(target), Some(wallets)) =
                (target, ReaderDal::new(storage).wallets_before(next).await?)
            {
                match crate::protocol::ProtocolBlock::prepare(client, indexer, target, wallets, body)
                    .await
                {
                    Ok(mut inputs) => {
                        let mut accepted = Vec::new();
                        for proof in inputs.proofs {
                            if proof.payload.input.pubdata.payload.input.l1_batch_index.0 > 0
                                && crate::protocol::ProtocolBlock::proof_is_authorized(
                                    &mut ReaderDal::new(storage),
                                    &proof,
                                    client.get_network(),
                                )
                                .await?
                            {
                                accepted.push(proof);
                            }
                        }
                        inputs.proofs = accepted;
                        block = Some(inputs);
                    }
                    Err(error) => {
                        if matches!(error.downcast_ref::<BitcoinError>(),Some(BitcoinError::InvalidBlockHeader {..} | BitcoinError::InvalidBlockBody(_))) {return Err(error)}
                    tracing::warn!(%error, next, "Protocol consumer held; header observation continues")
                    }
                }
            }
        }
        let deciding_bridge = block.as_ref().is_some_and(|block| block.configurations.iter()
            .any(|change| matches!(change.payload.input, via_btc_client::indexer::configuration::ConfigurationInput::Bridge(..))))
            || sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM via_btc_configuration WHERE role='Bridge' AND accepted IS NULL)")
                .instrument("bitcoin_pending_rotation").fetch_one(storage).await?;
        let completing_replay = !prepared.snapshot.state.replay_ready
            && (cursor.applied.header.height>=prepared.snapshot.state.cutoff.height
                || block.as_ref().is_some_and(|block|block.header.header.height>=prepared.snapshot.state.cutoff.height));
        let retained = if !prepared.snapshot.state.replay_ready && deciding_bridge {
            let txids = DB::retained_proof_txids(storage).await?;
            match RetainedProofs::prepare(client, &txids).await {
                Ok(retained) => match retained.revalidate(client).await {
                    Ok(()) => Some(retained),
                    Err(error) => { tracing::warn!(%error,"Retained proof source held"); None }
                },
                Err(error) => { tracing::warn!(%error,"Retained proof source unavailable"); None }
            }
        } else { Some(RetainedProofs(Vec::new())) };
        prepared.revalidate(client).await?;
        let mut tx = DB::OWNER.begin(storage).await?;
        DB::lock_source(&mut tx).await?;
        if matches!(prepared.transition, Transition::Idle | Transition::Append { .. }) {
            ReaderDal::new(&mut tx).finalize_ready_proofs(&prepared, threshold).await?;
        }
        if let Transition::Rewind { ancestor } = &prepared.transition {
            DB::delete_suffix(&mut tx, ancestor.header.height).await?;
        }
        prepared.apply_headers(&mut ReaderDal::new(&mut tx)).await?;
        if let Transition::Stop(reason) = &prepared.transition {
            tx.commit().await?;
            return Err(StoreError::Stopped(reason.clone()).into());
        }
        let progressed = !matches!(prepared.transition, Transition::Idle) || block.is_some();
        if let Some(mut block) = block {
            block.header = ReaderDal::new(&mut tx)
                .header_at(block.header.header.height)
                .await?
                .context("protocol header disappeared")?;
            DB::apply_protocol_block(&mut tx, &block).await?;
            for proof in &block.proofs { DB::save_proof(&mut tx, proof, block.header.id).await?; }
            let proofs = DB::canonical_proofs(&mut tx, block.header.header.height, crate::ProofScope::Interpretation).await?;
            block
                .complete(
                    &mut ReaderDal::new(&mut tx),
                    &proofs,
                    client.get_network(),
                    &cursor.oldest_needed,
                )
                .await?;
        }
        let through = ReaderDal::new(&mut tx)
            .consumer(crate::PROTOCOL_CONSUMER)
            .await?
            .context("protocol consumer disappeared")?
            .applied
            .header
            .height;
        let heights = sqlx::query_as::<_, (i64,)>(
            "SELECT DISTINCT l1_block_number FROM via_btc_configuration
            WHERE accepted IS NULL AND l1_block_number<=$1 ORDER BY l1_block_number",
        )
        .bind(i64::from(through))
        .instrument("bitcoin_undecided_configuration_heights")
        .fetch_all(&mut tx)
        .await?;
        for (height,) in heights {
            let height = u32::try_from(height)?;
            let proofs = DB::canonical_proofs(&mut tx, height, crate::ProofScope::Interpretation).await?;
            let proof_ids: Vec<_> = proofs.iter().map(|proof| proof.work.id).collect();
            let votes = ReaderDal::new(&mut tx).reader_votes(height, &proof_ids).await?;
            ReaderDal::new(&mut tx)
                .decide_configurations(
                    height,
                    &proofs,
                    &votes,
                    retained.as_ref().is_some_and(|retained|retained.complete_through(height)),
                    client.get_network(),
                    threshold,
                )
                .await?;
        }
        if !prepared.snapshot.state.replay_ready {
            if let Some(height) = DB::retained_boundary(&mut tx).await? {
                let mut reader = ReaderDal::new(&mut tx);
                let state = reader.state().await?.context("reader disappeared")?;
                if height>state.confirmed.height {
                    let protected = reader.header_at(height).await?.context(StoreError::Integrity("retained source header"))?;
                    let head = reader.get_highest_block().await?.context("reader head disappeared")?;
                    reader.advance_frontier(&head, &protected).await?;
                }
            }
        }
        let state = ReaderDal::new(&mut tx).state().await?.context("reader disappeared")?;
        let mature = prepared.selected.height.saturating_sub(prepared.depth.get()-1).min(state.confirmed.height);
        for change in ReaderDal::new(&mut tx).pending_protocol_changes(mature).await? {
            DB::materialize_protocol(&mut tx, &change).await?;
        }
        let ready = if !state.replay_ready && completing_replay {
            ReaderDal::new(&mut tx).complete_replay().await?
        } else { state.replay_ready };
        tx.commit().await?;
        let height = match &prepared.transition { Transition::Append { header, .. } => header.height,
            Transition::Rewind { ancestor } => ancestor.header.height, _ => prepared.snapshot.head.header.height };
        crate::metrics::METRICS.head[&DB::NAME].set(height as usize);
        crate::metrics::METRICS.frontier[&DB::NAME].set(state.confirmed.height as usize);
        crate::metrics::METRICS.replay_ready[&DB::NAME].set(usize::from(ready));
        crate::metrics::METRICS.stopped[&DB::NAME].set(0);
        crate::metrics::METRICS.consumer[&(DB::NAME, crate::PROTOCOL_CONSUMER)].set(through as usize);
        Ok(progressed)
    }.await;
    if let Err(error) = &result {
        if error.downcast_ref::<via_btc_client::indexer::replay::ReplayConflict>().is_some()
            || matches!(error.downcast_ref::<StoreError>(), Some(StoreError::Integrity(_)))
            || matches!(
                error.downcast_ref::<BitcoinError>(),
                Some(BitcoinError::InvalidBlockHeader { .. } | BitcoinError::InvalidBlockBody(_))
            )
        {
            let reason = format!("Bitcoin reader integrity conflict: {error:#}");
            let mut tx = DB::OWNER.begin(storage).await?;
            DB::lock_source(&mut tx).await?;
            ReaderDal::new(&mut tx).stop(&reason).await?;
            tx.commit().await?;
            crate::metrics::METRICS.stopped[&DB::NAME].set(1);
            crate::metrics::METRICS.replay_ready[&DB::NAME].set(0);
            return Err(StoreError::Stopped(reason).into());
        }
    }
    result
}

/// begin_authority holds reader ownership and source exclusion through the caller's action.
/// Fresh Core membership and mature dependencies are checked before returning the transaction.
/// Idea only from etcd's IsOwner transaction guard, which validates ownership in the protected transaction: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/client/v3/concurrency/mutex.go
pub async fn begin_authority<DB: crate::ReaderRole>(
    pool: &zksync_db_connection::connection_pool::ConnectionPool<DB>,
    client: &dyn BitcoinOps,
    dependencies: &[u32],
) -> anyhow::Result<zksync_db_connection::connection::Connection<'static, DB>> {
    let mut storage = pool.connection().await?;
    let snapshot =
        ReaderSnapshot::load(&mut ReaderDal::new(&mut storage), client.reorg_safe_depth()?).await?;
    let prepared = PreparedTransition::prepare(client, snapshot).await?;
    prepared.revalidate(client).await?;
    drop(storage);
    let mut tx = pool.start_transaction().await?;
    DB::OWNER.acquire(&mut tx).await?;
    DB::lock_authority_source(&mut tx).await?;
    let mut headers = Vec::with_capacity(dependencies.len());
    for height in dependencies {
        headers.push(
            ReaderDal::new(&mut tx).header_at(*height).await?.context(StoreError::AuthorityHeld)?,
        );
    }
    prepared.require_authority(&mut ReaderDal::new(&mut tx), &headers).await?;
    Ok(tx)
}

pub fn is_held(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<StoreError>(),
        Some(StoreError::AuthorityHeld | StoreError::HeadChanged | StoreError::SourceHeld(_))
    ) || error
        .downcast_ref::<BitcoinError>()
        .is_some_and(|error| matches!(error, BitcoinError::Rpc(_)))
}
