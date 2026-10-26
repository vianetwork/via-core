//! Native role queries project the same canonical proof identity into this shared owner.

use anyhow::Context;
use sqlx::{postgres::PgRow, Row};
use via_btc_client::{
    indexer::replay::{Included, ProofInput},
    types::ValidatorAttestationInput,
};
use zksync_db_connection::{connection::DbMarker, instrument::InstrumentExt};

use crate::{ReaderDal, ReaderRole, StoreError};

#[derive(Debug, Clone)]
pub struct ProofWork {
    pub id: i64,
    pub header_id: i64,
    pub proof: Included<ProofInput>,
}

impl ProofWork {
    pub fn from_row(row: &PgRow) -> anyhow::Result<Self> {
        let proof: Included<ProofInput> = serde_json::from_value(row.try_get("payload")?)
            .context(StoreError::Integrity("proof payload"))?;
        anyhow::ensure!(
            crate::block_ref(row.try_get("height")?, row.try_get("hash")?)?
                == via_reorg::chain::BlockRef {
                    height: proof.origin.height,
                    block_hash: proof.origin.hash
                },
            StoreError::Integrity("proof payload inclusion")
        );
        Ok(Self { id: row.try_get("id")?, header_id: row.try_get("header_id")?, proof })
    }
}

pub enum ProofQuorum {
    Incomplete,
    Complete(Option<bool>),
}

pub struct CanonicalProof {
    pub work: ProofWork,
    pub local_complete: bool,
    /// Q26 excludes only proofs finalized before the reset cutoff from rotation evidence.
    /// It neither grants cryptographic provenance nor bypasses ordinary action gates.
    pub rotation_exempt: bool,
}

impl CanonicalProof {
    pub fn from_row(row: &PgRow) -> anyhow::Result<Self> {
        Ok(Self {
            work: ProofWork::from_row(row)?,
            local_complete: row.try_get("local_complete")?,
            rotation_exempt: row.try_get("rotation_exempt")?,
        })
    }
}

pub struct CanonicalVote {
    pub proof_id: i64,
    pub height: u32,
    pub voter: String,
    pub approved: bool,
}

impl CanonicalVote {
    pub fn from_row(row: &PgRow) -> anyhow::Result<Self> {
        Ok(Self {
            proof_id: row.try_get("proof_id")?,
            height: row.try_get::<i64, _>("height")?.try_into()?,
            voter: row.try_get("voter")?,
            approved: row.try_get("approved")?,
        })
    }
}

impl<DB: DbMarker> ReaderDal<'_, '_, DB> {
    /// finalize_ready_proofs saves final result for each mature proof with complete local verdict and quorum, rechecking authority per proof.
    /// Runs every tick including empty blocks, so finalization never waits for next proof, and caller revalidates source before owner locks.
    /// Idea only from Hyperlane near-head confirm, which publishes only up to checked mature boundary with expected head: https://github.com/hyperlane-xyz/hyperlane-monorepo/blob/693d2d45df75135504e424c5a4d16761d0016f99/rust/main/agents/scraper/src/near_head/store.rs
    pub async fn finalize_ready_proofs(
        &mut self,
        prepared: &crate::transition::PreparedTransition,
        threshold: f64,
    ) -> anyhow::Result<u64>
    where
        DB: ReaderRole,
    {
        if !prepared.snapshot.state.replay_ready {
            return Ok(0);
        }
        if let Err(error) = prepared.require_authority(self, &[]).await {
            if matches!(error.downcast_ref::<StoreError>(), Some(StoreError::AuthorityHeld)) {
                return Ok(0);
            }
            return Err(error);
        }
        let Some(mature) = prepared.selected.height.checked_sub(prepared.depth.get() - 1) else {
            return Ok(0);
        };
        let through = mature.min(prepared.snapshot.state.confirmed.height);
        let proofs =
            DB::canonical_proofs(self.storage, through, crate::ProofScope::Finalization).await?;
        if proofs.is_empty() {
            return Ok(0);
        }
        let proof_ids: Vec<_> = proofs.iter().map(|proof| proof.work.id).collect();
        let votes = self.reader_votes(through, &proof_ids).await?;
        let mut saved = 0;
        for proof in proofs.iter().filter(|proof| proof.local_complete) {
            let work = &proof.work;
            if !self.proof_inputs_complete(work).await? {
                continue;
            }
            let ProofQuorum::Complete(Some(approved)) =
                self.proof_quorum(work, &votes, through, threshold).await?
            else {
                continue;
            };
            let origin = self
                .header_at(work.proof.origin.height)
                .await?
                .context("finalization origin disappeared")?;
            anyhow::ensure!(origin.id == work.header_id, StoreError::HeadChanged);
            prepared.require_authority(self, &[origin]).await?;
            saved += DB::finalize_proof(self.storage, work, approved).await?;
        }
        Ok(saved)
    }

    pub async fn authorize_vote_message(
        &mut self,
        message: &via_btc_client::types::InscriptionMessage,
        voter: &bitcoin::Address,
    ) -> anyhow::Result<()>
    where
        DB: ReaderRole,
    {
        let via_btc_client::types::InscriptionMessage::ValidatorAttestation(vote) = message else {
            anyhow::bail!(StoreError::Integrity("non-vote verifier publication"));
        };
        let state = self.state().await?.context(StoreError::AuthorityHeld)?;
        let proofs = DB::canonical_proofs(
            self.storage,
            state.confirmed.height,
            crate::ProofScope::Publication(vote.reference_txid),
        )
        .await?;
        let proof = proofs
            .iter()
            .find(|proof| proof.work.proof.origin.txid == vote.reference_txid)
            .context(StoreError::AuthorityHeld)?;
        let wallets = self
            .wallets_before(proof.work.proof.origin.height)
            .await?
            .context(StoreError::AuthorityHeld)?;
        anyhow::ensure!(wallets.verifiers.contains(voter), StoreError::AuthorityHeld);
        anyhow::ensure!(
            proof.local_complete && self.proof_inputs_complete(&proof.work).await?,
            StoreError::AuthorityHeld
        );
        Ok(())
    }

    pub async fn check_proof_source(
        &mut self,
        proof: &Included<ProofInput>,
        header_id: i64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("proof write outside transaction")
        );
        let parent = &proof.payload.input.pubdata.origin;
        let current = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM via_btc_headers p
            JOIN via_btc_headers d ON d.number=$4 AND d.hash=$5
            WHERE p.id=$1 AND p.number=$2 AND p.hash=$3 AND d.number<=p.number)",
        )
        .bind(header_id)
        .bind(i64::from(proof.origin.height))
        .bind(proof.origin.hash.to_string())
        .bind(i64::from(parent.height))
        .bind(parent.hash.to_string())
        .instrument("bitcoin_proof_source")
        .fetch_one(self.storage)
        .await?;
        anyhow::ensure!(current, StoreError::HeadChanged);
        Ok(())
    }

    /// proof_inputs_complete answers whether local proof check may run: header under frontier, protocol consumer past it, nothing earlier undecided.
    /// Local verdict save runs during replay because readiness waits for rotations and rotations wait for verdicts (Q24).
    /// Every irreversible action still waits for global readiness and fresh Core membership.
    /// Idea only from PostgreSQL table synchronization, which separates per-table SYNCDONE from subscription READY: https://github.com/postgres/postgres/blob/0019a1ecf4c7615a2570d21eeb4b32ae3b371f18/src/backend/replication/logical/tablesync.c
    pub async fn proof_inputs_complete(&mut self, work: &ProofWork) -> anyhow::Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM via_btc_headers h CROSS JOIN via_btc_reader r
            WHERE h.id=$1 AND h.number=$2 AND h.hash=$3 AND r.stopped_reason IS NULL AND r.confirmed_number>=h.number
            AND EXISTS(SELECT 1 FROM via_btc_consumers c JOIN via_btc_headers p ON p.id=c.header_id
                WHERE c.name=$4 AND p.number>=h.number)
            AND NOT EXISTS(SELECT 1 FROM via_btc_configuration WHERE l1_block_number<h.number AND accepted IS NULL))")
            .bind(work.header_id).bind(i64::from(work.proof.origin.height)).bind(work.proof.origin.hash.to_string())
            .bind(crate::PROTOCOL_CONSUMER).instrument("bitcoin_proof_inputs_complete").fetch_one(self.storage).await?)
    }

    pub async fn reader_votes(
        &mut self,
        through: u32,
        proof_ids: &[i64],
    ) -> anyhow::Result<Vec<CanonicalVote>> {
        let rows = sqlx::query(
            "SELECT v.votable_transaction_id AS proof_id,v.source_l1_block_number AS height,
            v.verifier_address AS voter,v.vote AS approved FROM via_votes v JOIN via_btc_headers h
            ON h.number=v.source_l1_block_number AND h.hash=v.source_l1_block_hash
            WHERE h.number<=$1 AND v.votable_transaction_id=ANY($2) AND v.bitcoin_payload IS NOT NULL
            ORDER BY h.number,v.transaction_index,v.message_index",
        )
        .bind(i64::from(through))
        .bind(proof_ids)
        .map(|row: PgRow| row)
        .instrument("bitcoin_canonical_votes")
        .fetch_all(self.storage)
        .await?;
        rows.iter().map(CanonicalVote::from_row).collect()
    }

    /// save_reader_vote stores one vote occurrence per source position and compares replay against stored payload.
    /// Voter derives from sender script, so forged verifier address inside payload counts nothing.
    /// Idea only from cw3-flex-multisig ballots, which keys one ballot per proposal and member: https://github.com/CosmWasm/cw-plus/blob/2be5732a555e98d4f7ba759d9f1cd5b8bf126972/contracts/cw3-flex-multisig/src/contract.rs
    pub async fn save_reader_vote(
        &mut self,
        vote: &Included<ValidatorAttestationInput>,
        proof: &ProofWork,
        network: bitcoin::Network,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("vote outside transaction")
        );
        let origin = &vote.origin;
        anyhow::ensure!(
            proof.proof.origin.txid == vote.payload.input.reference_txid
                && proof.proof.origin.height <= origin.height,
            StoreError::Integrity("vote proof inclusion")
        );
        let script = bitcoin::Script::from_bytes(
            vote.payload.sender_script.as_deref().context("vote has no sender")?,
        );
        let voter = bitcoin::Address::from_script(script, network)?.to_string();
        let Some(wallets) = self.wallets_before(proof.proof.origin.height).await? else {
            anyhow::bail!(StoreError::AuthorityHeld);
        };
        if !wallets.verifiers.iter().any(|member| member.to_string() == voter) {
            return Ok(());
        }
        let inserted = sqlx::query_scalar::<_, serde_json::Value>(
            "INSERT INTO via_votes(votable_transaction_id,verifier_address,vote,source_l1_block_number,source_l1_block_hash,
                txid,transaction_index,message_index,bitcoin_payload)
             SELECT $1,$2,$3,h.number,h.hash,$6,$7,$8,$9 FROM via_btc_headers h
             WHERE h.number=$4 AND h.hash=$5 AND EXISTS(SELECT 1 FROM via_btc_headers WHERE id=$10)
             ON CONFLICT DO NOTHING RETURNING bitcoin_payload",
        ).bind(proof.id).bind(&voter).bind(matches!(vote.payload.input.attestation,via_btc_client::types::Vote::Ok))
            .bind(i64::from(origin.height)).bind(origin.hash.to_string()).bind(origin.txid.to_string())
            .bind(i32::try_from(origin.transaction_index)?).bind(i32::try_from(origin.message_index)?)
            .bind(serde_json::to_value(vote)?).bind(proof.header_id)
            .instrument("bitcoin_save_vote_occurrence").fetch_optional(self.storage).await?;
        if inserted.is_none() {
            let saved = sqlx::query_scalar::<_, Option<serde_json::Value>>(
                "SELECT bitcoin_payload FROM via_votes WHERE votable_transaction_id=$1 AND verifier_address=$2",
            ).bind(proof.id).bind(voter).instrument("bitcoin_compare_member_vote")
                .fetch_optional(self.storage).await?.flatten()
                .context(via_btc_client::indexer::replay::ReplayConflict { field: "vote_key" })?;
            // Exact occurrence retry is idempotent.
            // Second occurrence from same member conflicts, including same decision (Q28).
            vote.compare(&serde_json::from_value(saved)?)?;
        }
        Ok(())
    }

    /// proof_quorum tallies canonical votes through frozen prefix against verifier set in force at proof's height.
    /// Action callers also cap maturity, so later votes never change decided prefix.
    pub async fn proof_quorum(
        &mut self,
        proof: &ProofWork,
        votes: &[CanonicalVote],
        through: u32,
        threshold: f64,
    ) -> anyhow::Result<ProofQuorum> {
        if proof.proof.origin.height > through {
            return Ok(ProofQuorum::Incomplete);
        }
        let Some(wallets) = self.wallets_before(proof.proof.origin.height).await? else {
            return Ok(ProofQuorum::Incomplete);
        };
        let voters: Vec<_> = wallets.verifiers.iter().map(ToString::to_string).collect();
        Ok(ProofQuorum::Complete(via_reorg::votes::tally(
            &proof.id,
            &voters,
            votes
                .iter()
                .filter(|v| v.height <= through)
                .map(|v| (v.proof_id, v.voter.clone(), v.approved)),
            threshold,
        )))
    }

    pub async fn rotation_decision(
        &mut self,
        proofs: &[CanonicalProof],
        inputs_complete: bool,
        votes: &[CanonicalVote],
        through: u32,
        threshold: f64,
    ) -> anyhow::Result<via_reorg::configuration::RotationDecision> {
        if !inputs_complete {
            return Ok(via_reorg::configuration::RotationDecision::NotReady);
        }
        let mut quorums = Vec::with_capacity(proofs.len());
        // Pre-#469 finalized proofs before reset cutoff stay unproven legacy history.
        // Expired testnet DA prevents re-verification. Later canonical proofs need evidence.
        // Mainnet starts with no such rows. Exception grants no other authority.
        for proof in proofs.iter().filter(|proof| !proof.rotation_exempt) {
            if !proof.local_complete {
                return Ok(via_reorg::configuration::RotationDecision::NotReady);
            }
            match self.proof_quorum(&proof.work, votes, through, threshold).await? {
                ProofQuorum::Incomplete => {
                    return Ok(via_reorg::configuration::RotationDecision::NotReady)
                }
                ProofQuorum::Complete(quorum) => quorums.push(quorum),
            }
        }
        Ok(via_reorg::configuration::decide_rotation(true, quorums))
    }
}
