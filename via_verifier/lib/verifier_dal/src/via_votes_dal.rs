use sqlx::Row;
use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::H256;

use crate::{models::storage_vote::CanonicalChainStatus, Verifier, VerifierDal};

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

/// Retained proof inputs survive loss of their current projection, including an ignored competing proof.
/// Idea: retain rollback metadata with its data, as in Graph's concrete store adapter:
/// https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofMembership {
    RetainedOnly,
    Canonical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofAdmission {
    Admitted { rewound_to: Option<u32> },
    Ignored,
    ContinuityGap { expected_batch: u32, canonical_parent: Option<H256> },
}

const NEXT_VERIFICATION: &str = "SELECT v.id FROM via_votable_transactions v
    WHERE v.l1_batch_status IS NULL AND (v.l1_batch_number=1 OR EXISTS(
        SELECT 1 FROM via_votable_transactions parent WHERE parent.l1_batch_number=v.l1_batch_number-1
        AND parent.l1_batch_hash=v.prev_l1_batch_hash AND parent.l1_batch_status=TRUE))
    ORDER BY v.l1_batch_number,v.proof_l1_block_number,v.id LIMIT 1";

impl ViaVotesDal<'_, '_> {
    pub async fn lock_verdict_inputs(&mut self) -> anyhow::Result<()> {
        self.storage.via_indexer_dal().lock_source_writer().await?;
        Ok(())
    }

    /// Structural read-set validation includes empty selections and complete ordered prefixes.
    /// Adapted from Aptos CapturedReads, whose VM / multiversion maps do not fit SQLx:
    /// https://github.com/aptos-labs/aptos-core/blob/302fd611d6f007ee8f3a6f7a2d60347db2293cee/aptos-move/block-executor/src/captured_reads.rs#L515-L546
    pub async fn verification_snapshot(
        &mut self,
        id: i64,
        deposit_count: usize,
    ) -> DalResult<Option<serde_json::Value>> {
        let count = i64::try_from(deposit_count).map_err(|err| {
            Instrumented::new("verification_snapshot").arg_error("deposit_count", err)
        })?;
        let query=format!("WITH candidate AS(SELECT * FROM via_votable_transactions WHERE id=$1),
            deposit_prefix AS(SELECT t.* FROM via_transactions t,candidate c
                WHERE t.status IS NULL OR t.l1_batch_number=c.l1_batch_number ORDER BY t.priority_id LIMIT $2)
            SELECT jsonb_build_object(
                'candidate',to_jsonb(c),
                'selected_candidate',({NEXT_VERIFICATION}),
                'inclusion',to_jsonb(i),'proof',to_jsonb(p),'historical_set',to_jsonb(w),'header',to_jsonb(h),
                'parents_and_siblings',COALESCE((SELECT jsonb_agg(to_jsonb(parent) ORDER BY parent.id)
                    FROM via_votable_transactions parent WHERE parent.l1_batch_number=c.l1_batch_number
                    OR (parent.l1_batch_number=c.l1_batch_number-1 AND parent.l1_batch_hash=c.prev_l1_batch_hash)),'[]'::JSONB),
                'votes',COALESCE((SELECT jsonb_agg(jsonb_build_object('evidence',to_jsonb(e),'inclusion',to_jsonb(v)) ORDER BY v.block_height,v.tx_index,v.message_index,v.id)
                    FROM via_vote_evidence e JOIN via_vote_inclusions v ON v.vote_id=e.id
                    JOIN via_l1_blocks b ON b.number=v.block_height AND b.hash=v.block_hash
                    WHERE e.proof_txid=p.proof_txid AND v.revoked_at IS NULL),'[]'::JSONB),
                'deposit_prefix',COALESCE((SELECT jsonb_agg(to_jsonb(d) ORDER BY d.priority_id) FROM deposit_prefix d),'[]'::JSONB),
                'deposit_sources',COALESCE((SELECT jsonb_agg(to_jsonb(d) ORDER BY d.id) FROM via_deposit_inclusions d
                    JOIN via_l1_blocks b ON b.number=d.block_height AND b.hash=d.block_hash
                    WHERE d.revoked_at IS NULL AND d.operation_hash IN(SELECT canonical_tx_hash FROM deposit_prefix)),'[]'::JSONB),
                'protocol_versions',COALESCE((SELECT jsonb_agg(to_jsonb(version) ORDER BY version.id) FROM protocol_versions version),'[]'::JSONB),
                'protocol_patches',COALESCE((SELECT jsonb_agg(to_jsonb(patch) ORDER BY patch.minor,patch.patch) FROM protocol_patches patch),'[]'::JSONB),
                'upgrade_evidence',COALESCE((SELECT jsonb_agg(to_jsonb(upgrade) ORDER BY upgrade.id) FROM via_upgrade_evidence upgrade),'[]'::JSONB),
                'upgrade_inclusions',COALESCE((SELECT jsonb_agg(to_jsonb(upgrade) ORDER BY upgrade.id) FROM via_upgrade_inclusions upgrade),'[]'::JSONB),
                'store_mode',(SELECT to_jsonb(mode) FROM via_verifier_store_mode mode WHERE mode.id=1),
                'progress',(SELECT jsonb_build_object('module',progress.module,'height',progress.last_indexer_l1_block) FROM via_indexer_metadata progress WHERE progress.module='via_btc_watch'),
                'positions',COALESCE((SELECT jsonb_agg(jsonb_build_object('group',position.group_name,'height',position.height,'hash',position.hash,'certificate',position.certificate_id) ORDER BY position.group_name) FROM via_btc_scan_positions position),'[]'::JSONB),
                'barriers',COALESCE((SELECT jsonb_agg(to_jsonb(barrier)) FROM via_l1_reorg barrier),'[]'::JSONB)
            ) FROM candidate c JOIN via_proof_inclusions i ON i.id=c.reader_proof_inclusion_id
            JOIN via_proof_evidence p ON p.id=i.proof_id JOIN via_wallets w ON w.id=i.verifier_set_id
            JOIN via_btc_headers h ON h.hash=i.block_hash AND h.height=i.block_height");
        sqlx::query_scalar(&query)
            .bind(id)
            .bind(count)
            .instrument("verification_snapshot")
            .fetch_optional(self.storage)
            .await
    }

    /// Each checked inclusion gets its own verdict incarnation. Equal replay preserves completion.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_votable_transaction(
        &mut self,
        l1_batch_number: u32,
        l1_batch_hash: H256,
        prev_l1_batch_hash: H256,
        da_identifier: String,
        proof_reveal_tx_id: H256,
        proof_blob_id: String,
        pubdata_reveal_tx_id: String,
        pubdata_blob_id: String,
        reader_inclusion_id: i64,
        membership: ProofMembership,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("insert_votable_transaction")
                .constraint_error(error)
        })?;
        let written=sqlx::query("INSERT INTO via_proof_verdicts(l1_batch_number,l1_batch_hash,prev_l1_batch_hash,
            proof_reveal_tx_id,da_identifier,proof_blob_id,pubdata_reveal_tx_id,pubdata_blob_id,
            source_l1_block_number,source_l1_block_hash,proof_l1_block_number,proof_l1_block_hash,reader_proof_inclusion_id)
            SELECT $1,$2,$3,$4,$5,$6,$7,$8,i.block_height,i.block_hash,i.block_height,i.block_hash,i.id
            FROM via_proof_inclusions i JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE i.id=$9 AND i.revoked_at IS NULL
            ON CONFLICT(reader_proof_inclusion_id) DO UPDATE SET id=CASE WHEN
            via_btc_immutable_payload(jsonb_build_object('batch',via_proof_verdicts.l1_batch_number,
                'batch_hash',via_proof_verdicts.l1_batch_hash,'parent_hash',via_proof_verdicts.prev_l1_batch_hash,
                'proof_txid',via_proof_verdicts.proof_reveal_tx_id,'proof_blob',via_proof_verdicts.proof_blob_id,
                'batch_txid',via_proof_verdicts.pubdata_reveal_tx_id,'batch_blob',via_proof_verdicts.pubdata_blob_id,
                'da_identifier',via_proof_verdicts.da_identifier),
                jsonb_build_object('batch',EXCLUDED.l1_batch_number,'batch_hash',EXCLUDED.l1_batch_hash,
                'parent_hash',EXCLUDED.prev_l1_batch_hash,'proof_txid',EXCLUDED.proof_reveal_tx_id,
                'proof_blob',EXCLUDED.proof_blob_id,'batch_txid',EXCLUDED.pubdata_reveal_tx_id,
                'batch_blob',EXCLUDED.pubdata_blob_id,'da_identifier',EXCLUDED.da_identifier)) IS NOT NULL
                THEN via_proof_verdicts.id END")
            .bind(i64::from(l1_batch_number)).bind(l1_batch_hash.as_bytes()).bind(prev_l1_batch_hash.as_bytes())
            .bind(proof_reveal_tx_id.as_bytes()).bind(da_identifier).bind(proof_blob_id).bind(pubdata_reveal_tx_id)
            .bind(pubdata_blob_id).bind(reader_inclusion_id).instrument("insert_proof_verdict_inputs")
            .execute(&mut *self.storage).await?;
        if written.rows_affected() != 1 {
            return Err(Instrumented::new("insert_proof_verdict_inputs")
                .constraint_error(anyhow::anyhow!("proof incarnation is no longer canonical")));
        }
        if membership == ProofMembership::Canonical {
            self.admit_votable_transaction(reader_inclusion_id).await?;
        }
        Ok(())
    }

    /// Ignored proofs retain their linked batch inputs so undo can rebuild the prior L2 chain.
    /// Admission follows the node's continuity checks and never allocates a second verdict incarnation.
    pub async fn admit_votable_transaction(&mut self, reader_inclusion_id: i64) -> DalResult<()> {
        self.storage
            .via_indexer_dal()
            .lock_source_writer()
            .await
            .map_err(|error| Instrumented::new("admit_proof_membership").constraint_error(error))?;
        let written=sqlx::query("UPDATE via_proof_inclusions i SET l2_eligible=TRUE WHERE i.id=$1 AND i.revoked_at IS NULL
            AND EXISTS(SELECT 1 FROM via_l1_blocks b WHERE b.number=i.block_height AND b.hash=i.block_hash)
            AND EXISTS(SELECT 1 FROM via_proof_verdicts v WHERE v.reader_proof_inclusion_id=i.id)")
            .bind(reader_inclusion_id).instrument("admit_proof_membership").execute(self.storage).await?;
        if written.rows_affected() != 1 {
            return Err(Instrumented::new("admit_proof_membership")
                .constraint_error(anyhow::anyhow!("proof incarnation is no longer canonical")));
        }
        Ok(())
    }

    /// Live and stopped replay retain one admission rule; source-caused exclusions restore the preceding version.
    /// https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/relational.rs
    pub async fn admit_recorded_proof(
        &mut self,
        inclusion_id: i64,
        proof_txid: H256,
        batch: u32,
        parent: H256,
    ) -> DalResult<ProofAdmission> {
        self.storage
            .via_indexer_dal()
            .lock_source_writer()
            .await
            .map_err(|error| Instrumented::new("admit_recorded_proof").constraint_error(error))?;
        if batch == 0 {
            return Ok(ProofAdmission::Ignored);
        }
        let exact_replay = self.get_votable_transaction_id(proof_txid.as_bytes()).await?.is_some();
        if !exact_replay && batch == 1 && self.batch_exists(1).await? {
            return Ok(ProofAdmission::Ignored);
        }
        let mut rewound_to = None;
        if !exact_replay && batch > 1 {
            let Some(last) = self.get_last_batch_in_canonical_chain().await? else {
                return Ok(ProofAdmission::ContinuityGap {
                    expected_batch: 1,
                    canonical_parent: None,
                });
            };
            if last.0 + 1 != batch {
                if !self
                    .get_parent_batch_exists_for_l1_batch(i64::from(batch), parent.as_bytes())
                    .await?
                {
                    return Ok(ProofAdmission::ContinuityGap {
                        expected_batch: last.0 + 1,
                        canonical_parent: Some(H256::from_slice(&last.1)),
                    });
                }
                let retained = batch - 1;
                let mut transaction = self.storage.start_transaction().await?;
                transaction
                    .via_withdrawal_dal()
                    .invalidate_withdrawal_batches_from(batch)
                    .await
                    .map_err(|error| {
                        Instrumented::new("admit_recorded_proof").constraint_error(error)
                    })?;
                transaction
                    .via_votes_dal()
                    .supersede_proofs_after_l2_batch(i64::from(retained), inclusion_id)
                    .await?;
                transaction.via_transactions_dal().reset_transactions(i64::from(retained)).await?;
                transaction.commit().await?;
                rewound_to = Some(retained);
            } else if last.1 != parent.0 {
                return Ok(ProofAdmission::Ignored);
            }
        }
        self.admit_votable_transaction(inclusion_id).await?;
        Ok(ProofAdmission::Admitted { rewound_to })
    }

    pub async fn get_votable_transaction_id(&mut self, proof: &[u8]) -> DalResult<Option<i64>> {
        sqlx::query_scalar("SELECT CASE WHEN COUNT(*)=1 THEN MIN(id) END FROM via_votable_transactions WHERE proof_reveal_tx_id=$1")
            .bind(proof).instrument("current_proof_incarnation").fetch_one(self.storage).await
    }

    /// First batch whose proof or retained vote lies in the reverted suffix.
    pub async fn get_l1_batch_number_affected_by_source_reorg(
        &mut self,
        last_retained_l1_block: i64,
    ) -> DalResult<Option<i64>> {
        sqlx::query("SELECT MIN(v.l1_batch_number) AS batch FROM via_proof_verdicts v
            JOIN via_current_proof_inclusions p ON p.id=v.reader_proof_inclusion_id
            JOIN via_proof_evidence proof ON proof.id=p.proof_id
            WHERE (p.block_height>$1 OR EXISTS(SELECT 1 FROM via_vote_evidence e
                JOIN via_vote_inclusions i ON i.vote_id=e.id WHERE e.proof_txid=proof.proof_txid
                AND i.block_height>$1 AND i.revoked_at IS NULL))")
            .bind(last_retained_l1_block)
            .map(|row: sqlx::postgres::PgRow| row.get::<Option<i64>, _>("batch"))
            .instrument("get_l1_batch_number_affected_by_source_reorg")
            .fetch_one(self.storage)
            .await
    }

    /// First batch whose proof itself lies in the reverted suffix.
    pub async fn get_l1_batch_number_affected_by_proof_reorg(
        &mut self,
        last_retained_l1_block: i64,
    ) -> DalResult<Option<i64>> {
        sqlx::query("SELECT MIN(v.l1_batch_number) AS batch FROM via_proof_verdicts v JOIN via_current_proof_inclusions i ON i.id=v.reader_proof_inclusion_id WHERE i.block_height>$1")
            .bind(last_retained_l1_block)
            .map(|row: sqlx::postgres::PgRow| row.get::<Option<i64>, _>("batch"))
            .instrument("get_l1_batch_number_affected_by_proof_reorg")
            .fetch_one(self.storage)
            .await
    }

    /// Revocation preserves local admissions and completed verdicts, including on a vote-only fork.
    pub async fn revert_votes_after_l1_block(&mut self, height: i64) -> DalResult<Vec<i64>> {
        let ids=sqlx::query("SELECT DISTINCT v.id FROM via_proof_verdicts v
            JOIN via_proof_inclusions p ON p.id=v.reader_proof_inclusion_id
            JOIN via_proof_evidence e ON e.id=p.proof_id
            JOIN via_vote_evidence vote ON vote.proof_txid=e.proof_txid
            JOIN via_vote_inclusions i ON i.vote_id=vote.id
            WHERE p.block_height<=$1 AND p.revoked_at IS NULL AND i.block_height>$1 AND i.revoked_at IS NULL")
            .bind(height).try_map(|row:sqlx::postgres::PgRow| row.try_get::<i64,_>("id"))
            .instrument("retained_proofs_with_orphan_votes").fetch_all(&mut *self.storage).await?;
        self.revoke_reader_inclusions_after(height).await?;
        Ok(ids)
    }

    /// All roles use exact-proof / historical-set quorum; local votes are never Bitcoin authority.
    pub async fn finalize_transaction_if_needed(
        &mut self,
        id: i64,
        threshold: f64,
    ) -> DalResult<bool> {
        self.storage
            .via_indexer_dal()
            .lock_source_writer()
            .await
            .map_err(|error| Instrumented::new("finalize_reader_proof").constraint_error(error))?;
        let previous: Option<bool> =
            sqlx::query_scalar("SELECT is_finalized FROM via_votable_transactions WHERE id=$1")
                .bind(id)
                .instrument("reader_previous_finalization")
                .fetch_optional(&mut *self.storage)
                .await?
                .flatten();
        let through =
            self.storage.via_indexer_dal().get_last_processed_l1_block("via_btc_watch").await?;
        self.recompute_reader_finalization(
            u32::try_from(through).map_err(|err| {
                zksync_db_connection::instrument::Instrumented::new("reader_finalization")
                    .arg_error("through", err)
            })?,
            threshold,
        )
        .await?;
        let current: Option<bool> =
            sqlx::query_scalar("SELECT is_finalized FROM via_votable_transactions WHERE id=$1")
                .bind(id)
                .instrument("reader_current_finalization")
                .fetch_optional(self.storage)
                .await?
                .flatten();
        Ok(previous.is_none() && current == Some(true))
    }

    pub async fn get_last_finalized_l1_batch(&mut self) -> DalResult<Option<u32>> {
        let row = sqlx::query!(
            r#"
            SELECT
                MAX(l1_batch_number) AS max_batch_number
            FROM
                via_votable_transactions
            WHERE
                is_finalized = TRUE
            "#
        )
        .instrument("get_last_inserted_block")
        .fetch_one(self.storage)
        .await?;

        Ok(row.max_batch_number.map(|n| n as u32))
    }

    pub async fn get_last_voted_l1_batch(&mut self) -> DalResult<u32> {
        let row = sqlx::query!(
            r#"
            SELECT
                MAX(l1_batch_number) AS max_batch_number
            FROM
                via_votable_transactions
            WHERE
                l1_batch_status IS NOT NULL
            "#
        )
        .instrument("get_last_voted_l1_batch")
        .fetch_one(self.storage)
        .await?;
        Ok(row.max_batch_number.unwrap_or(0) as u32)
    }

    pub async fn get_last_votable_l1_batch(&mut self) -> DalResult<u32> {
        let row = sqlx::query!(
            r#"
            SELECT
                MAX(l1_batch_number) AS max_batch_number
            FROM
                via_votable_transactions
            "#
        )
        .instrument("get_last_votable_l1_batch")
        .fetch_one(self.storage)
        .await?;
        Ok(row.max_batch_number.unwrap_or(0) as u32)
    }

    /// Reads canonical indexed and positively approved batch heights from one snapshot.
    /// These maxima include development and historical approvals, not a cleared prefix.
    pub async fn get_verification_watermarks(&mut self) -> DalResult<(u32, u32)> {
        let row = sqlx::query!(
            r#"
            SELECT
                MAX(l1_batch_number) AS indexed,
                MAX(l1_batch_number) FILTER (WHERE l1_batch_status = TRUE) AS approved
            FROM via_votable_transactions
            WHERE is_finalized IS DISTINCT FROM FALSE
            "#
        )
        .instrument("get_verification_watermarks")
        .fetch_one(self.storage)
        .await?;
        Ok((row.indexed.unwrap_or(0) as u32, row.approved.unwrap_or(0) as u32))
    }

    /// Only the captured current incarnation can acquire a positive completed verdict.
    pub async fn mark_proof_verified(
        &mut self,
        id: i64,
        inputs: &serde_json::Value,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("mark_proof_verified")
                .constraint_error(error)
        })?;
        let updated: Option<i64> = sqlx::query_scalar(
            "UPDATE via_proof_verdicts v SET
            l1_batch_status=TRUE,updated_at=NOW(),completed_input_identity=$2
            WHERE v.id=$1 AND v.l1_batch_status IS NULL
                AND EXISTS(SELECT 1 FROM via_votable_transactions current WHERE current.id=v.id)
            RETURNING v.id",
        )
        .bind(id)
        .bind(inputs)
        .instrument("mark_proof_verified")
        .fetch_optional(self.storage)
        .await?;
        if updated.is_none() {
            return Err(Instrumented::new("mark_proof_verified").constraint_error(
                anyhow::anyhow!(
                    "proof incarnation is stale or already has a different completed verdict"
                ),
            ));
        }
        Ok(())
    }

    /// https://github.com/risc0/risc0/blob/218e3bc4a8ffcd203a9cd4e46f921bf60aa7e2bd/risc0/zkvm/src/receipt.rs#L443-L445
    pub async fn mark_unverified_dev(&mut self, votable_transaction_id: i64) -> anyhow::Result<()> {
        self.storage.via_indexer_dal().lock_source_writer().await?;
        let updated = sqlx::query(
            "UPDATE via_proof_verdicts SET unverified_dev = TRUE WHERE id = $1 \
             AND EXISTS (SELECT 1 FROM via_verifier_store_mode WHERE id = 1 AND proof_verification_dev_mode)",
        )
        .bind(votable_transaction_id)
        .instrument("mark_unverified_dev")
        .execute(self.storage)
        .await?
        .rows_affected();
        anyhow::ensure!(
            updated == 1,
            "votable transaction {votable_transaction_id} is missing or the store is not designated for development"
        );
        Ok(())
    }

    pub async fn get_first_non_finalized_l1_batch_in_canonical_inscription_chain(
        &mut self,
    ) -> DalResult<Option<i64>> {
        let id = sqlx::query_scalar!(
            r#"
            WITH last_finalized AS (
                SELECT
                    l1_batch_number,
                    l1_batch_hash
                FROM via_votable_transactions
                WHERE is_finalized = TRUE
                ORDER BY l1_batch_number DESC
                LIMIT 1
            )
            SELECT v.id
            FROM via_votable_transactions v
            LEFT JOIN last_finalized lf ON v.prev_l1_batch_hash = lf.l1_batch_hash
            WHERE v.is_finalized IS NULL
            AND (
                (v.l1_batch_number = 1 AND NOT EXISTS(SELECT 1 FROM last_finalized))
                OR 
                (lf.l1_batch_hash IS NOT NULL AND v.l1_batch_number = lf.l1_batch_number + 1)
            )
            ORDER BY v.l1_batch_number ASC
            LIMIT 1;
            "#,
        )
        .instrument("get_first_non_finalized_l1_batch_in_canonical_inscription_chain")
        .fetch_optional(self.storage)
        .await?;

        Ok(id)
    }

    /// Admission uses the proof's historical signer set and canonical observations, including our own vote.
    /// https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/validation.go
    pub async fn may_create_vote(&mut self, id: i64, verifier: &str) -> DalResult<bool> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM via_votable_transactions v
            JOIN via_proof_inclusions proof ON proof.id=v.reader_proof_inclusion_id
            JOIN via_proof_evidence evidence ON evidence.id=proof.proof_id
            JOIN via_wallets historical ON historical.id=proof.verifier_set_id
            WHERE v.id=$1 AND historical.role='Verifier' AND $2=ANY(string_to_array(historical.address,','))
            AND NOT EXISTS(SELECT 1 FROM via_vote_evidence vote
                JOIN via_vote_inclusions inclusion ON inclusion.vote_id=vote.id
                JOIN via_l1_blocks canonical ON canonical.number=inclusion.block_height AND canonical.hash=inclusion.block_hash
                WHERE vote.proof_txid=evidence.proof_txid AND vote.verifier_address=$2 AND inclusion.revoked_at IS NULL))")
            .bind(id).bind(verifier).instrument("may_create_vote").fetch_one(self.storage).await
    }

    pub async fn get_verifier_vote_status(
        &mut self,
        votable_transaction_id: i64,
    ) -> DalResult<Option<(bool, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            SELECT
                l1_batch_status,
                proof_reveal_tx_id
            FROM
                via_votable_transactions
            WHERE
                id = $1
                AND l1_batch_status IS NOT NULL
            LIMIT
                1
            "#,
            votable_transaction_id
        )
        .instrument("get_verifier_vote_status")
        .fetch_optional(self.storage)
        .await?;

        let result = row.map(|r| {
            let l1_batch_status = r.l1_batch_status.unwrap_or_default();
            let proof_reveal_tx_id = r.proof_reveal_tx_id;
            (l1_batch_status, proof_reveal_tx_id)
        });

        Ok(result)
    }

    pub async fn get_first_not_verified_l1_batch_in_canonical_inscription_chain(
        &mut self,
    ) -> DalResult<Option<(i64, Vec<u8>)>> {
        let query=format!("SELECT l1_batch_number,proof_reveal_tx_id FROM via_votable_transactions WHERE id=({NEXT_VERIFICATION})");
        sqlx::query(&query)
            .try_map(|row: sqlx::postgres::PgRow| {
                Ok((row.try_get("l1_batch_number")?, row.try_get("proof_reveal_tx_id")?))
            })
            .instrument("next_proof_verification")
            .fetch_optional(self.storage)
            .await
    }

    /// Get the first rejected l1 batch (failed zk proof).
    pub async fn get_first_rejected_l1_batch(&mut self) -> DalResult<Option<(i64, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            SELECT
                l1_batch_number,
                l1_batch_hash
            FROM
                via_votable_transactions
            WHERE
                is_finalized = FALSE
                AND l1_batch_status = FALSE
            ORDER BY
                l1_batch_number ASC
            LIMIT
                1
            "#
        )
        .instrument("get_first_rejected_l1_batch")
        .fetch_optional(self.storage)
        .await?;

        let result = row.map(|r| (r.l1_batch_number, r.l1_batch_hash));

        Ok(result)
    }

    pub async fn get_last_batch_in_canonical_chain(&mut self) -> DalResult<Option<(u32, Vec<u8>)>> {
        let row = sqlx::query!(
            r#"
            WITH RECURSIVE
            canonical_chain AS (
                (
                    SELECT
                        *
                    FROM
                        via_votable_transactions
                    WHERE
                        is_finalized = TRUE
                        OR is_finalized IS NULL
                    ORDER BY
                        l1_batch_number DESC
                    LIMIT
                        1
                )
                UNION ALL
                SELECT
                    vt.*
                FROM
                    via_votable_transactions vt
                JOIN canonical_chain cc ON
                    vt.prev_l1_batch_hash = cc.l1_batch_hash
                    AND vt.l1_batch_number = cc.l1_batch_number + 1
                    AND (
                        vt.l1_batch_status IS NULL
                        OR vt.l1_batch_status = TRUE
                    )
            )
            
            SELECT
                l1_batch_number,
                l1_batch_hash
            FROM
                canonical_chain
            ORDER BY
                l1_batch_number DESC
            LIMIT
                1
            "#
        )
        .instrument("get_last_batch_in_canonical_chain")
        .fetch_optional(self.storage)
        .await?;

        Ok(row.and_then(|r| Some((r.l1_batch_number? as u32, r.l1_batch_hash?))))
    }

    /// Verify that the canonical chain is valid and continuous
    pub async fn verify_canonical_chain(&mut self) -> DalResult<CanonicalChainStatus> {
        let result = sqlx::query!(
            r#"
            WITH RECURSIVE
            canonical_chain AS (
                (
                    SELECT
                        *,
                        1::BIGINT AS chain_position,
                        TRUE AS is_valid_link
                    FROM
                        via_votable_transactions
                    WHERE
                        l1_batch_number = 1
                    ORDER BY
                        created_at ASC -- Take the first one if multiple exist
                    LIMIT
                        1
                )
                UNION ALL
                SELECT
                    vt.*,
                    cc.chain_position + 1 AS chain_position,
                    (
                        vt.prev_l1_batch_hash = cc.l1_batch_hash
                        AND vt.l1_batch_number = cc.l1_batch_number + 1
                    ) AS is_valid_link
                FROM
                    via_votable_transactions vt
                JOIN canonical_chain cc
                    ON
                        vt.prev_l1_batch_hash = cc.l1_batch_hash
                        AND vt.l1_batch_number = cc.l1_batch_number + 1
                        AND (
                            vt.l1_batch_status IS NULL
                            OR vt.l1_batch_status = TRUE
                        )
                WHERE
                    cc.is_valid_link = TRUE -- Only continue if previous link was valid
            ),
            
            chain_stats AS (
                SELECT
                    COUNT(*) AS total_batches,
                    MAX(l1_batch_number) AS max_batch_number,
                    MIN(l1_batch_number) AS min_batch_number,
                    BOOL_AND(is_valid_link) AS all_links_valid,
                    ARRAY_AGG(
                        l1_batch_number
                        ORDER BY
                            l1_batch_number
                    ) AS batch_numbers
                FROM
                    canonical_chain
            ),
            
            expected_sequence AS (
                SELECT
                    GENERATE_SERIES(
                        (
                            SELECT
                                min_batch_number
                            FROM
                                chain_stats
                        ),
                        (
                            SELECT
                                max_batch_number
                            FROM
                                chain_stats
                        )
                    ) AS expected_batch
            ),
            
            missing_batches AS (
                SELECT
                    ARRAY_AGG(expected_batch) AS missing
                FROM
                    expected_sequence es
                WHERE
                    NOT EXISTS (
                        SELECT
                            1
                        FROM
                            canonical_chain cc
                        WHERE
                            cc.l1_batch_number = es.expected_batch
                    )
            )
            
            SELECT
                cs.total_batches,
                cs.max_batch_number,
                cs.min_batch_number,
                cs.all_links_valid,
                cs.batch_numbers,
                mb.missing,
                (
                    cs.total_batches = cs.max_batch_number - cs.min_batch_number + 1
                ) AS is_continuous,
                (
                    SELECT
                        COUNT(*)
                    FROM
                        via_votable_transactions
                ) AS total_transactions_in_db
            FROM
                chain_stats cs,
                missing_batches mb
            "#
        )
        .instrument("verify_canonical_chain")
        .fetch_optional(self.storage)
        .await?;

        match result {
            Some(row) => {
                let status = CanonicalChainStatus {
                    is_valid: row.all_links_valid.unwrap_or(false)
                        && row.is_continuous.unwrap_or(false)
                        && row.missing.is_none(),
                    total_canonical_batches: row.total_batches.unwrap_or(0),
                    max_batch_number: row.max_batch_number.map(|n| n as u32),
                    min_batch_number: row.min_batch_number.map(|n| n as u32),
                    missing_batches: row
                        .missing
                        .unwrap_or_default()
                        .into_iter()
                        .map(|n| n as u32)
                        .collect(),
                    batch_sequence: row
                        .batch_numbers
                        .unwrap_or_default()
                        .into_iter()
                        .map(|n| n as u32)
                        .collect(),
                    total_transactions_in_db: row.total_transactions_in_db.unwrap_or(0),
                    has_genesis: row.min_batch_number == Some(1),
                };
                Ok(status)
            }
            None => {
                // No canonical chain found (no batch 1)
                Ok(CanonicalChainStatus {
                    is_valid: false,
                    total_canonical_batches: 0,
                    max_batch_number: None,
                    min_batch_number: None,
                    missing_batches: vec![],
                    batch_sequence: vec![],
                    total_transactions_in_db: 0,
                    has_genesis: false,
                })
            }
        }
    }

    /// Check if a batch with the given number already exists
    pub async fn batch_exists(&mut self, l1_batch_number: u32) -> DalResult<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1 
                FROM via_votable_transactions 
                WHERE l1_batch_number = $1
            )
            "#,
            i64::from(l1_batch_number)
        )
        .instrument("batch_exists")
        .fetch_one(self.storage)
        .await?;

        Ok(exists.unwrap_or(false))
    }

    pub async fn proof_reveal_tx_exists(&mut self, proof_reveal_tx_id: &[u8]) -> DalResult<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1 
                FROM via_votable_transactions 
                WHERE proof_reveal_tx_id = $1
            )
            "#,
            proof_reveal_tx_id
        )
        .instrument("proof_reveal_tx_exists")
        .fetch_one(self.storage)
        .await?;

        Ok(exists.unwrap_or(false))
    }

    /// A source-displaced version can return only with retained inputs; damaged or unproved L2 output stays withheld.
    /// https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/relational.rs
    pub async fn invalidate_l2_inputs_after_reorg(
        &mut self,
        first_batch: i64,
        retained_height: i64,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            Instrumented::new("invalidate_l2_inputs_after_reorg").constraint_error(error)
        })?;
        sqlx::query("UPDATE via_proof_inclusions inclusion SET l2_eligible=FALSE
            FROM via_proof_evidence proof WHERE proof.id=inclusion.proof_id
            AND proof.l1_batch_number>=$1 AND inclusion.l2_eligible
            AND NOT (inclusion.block_height<=$2 AND EXISTS(
                SELECT 1 FROM via_proof_l2_exclusions exclusion
                JOIN via_proof_inclusions cause ON cause.id=exclusion.excluding_inclusion_id
                JOIN via_l1_blocks canonical ON canonical.number=cause.block_height AND canonical.hash=cause.block_hash
                JOIN via_proof_verdicts verdict ON verdict.reader_proof_inclusion_id=inclusion.id
                WHERE exclusion.excluded_inclusion_id=inclusion.id AND cause.block_height>$2 AND cause.revoked_at IS NULL
                AND (verdict.l1_batch_status IS NULL OR (verdict.completed_input_identity IS NOT NULL AND NOT EXISTS(
                    SELECT 1 FROM jsonb_array_elements(
                        COALESCE(verdict.completed_input_identity->'deposit_sources','[]'::JSONB)
                        || COALESCE(verdict.completed_input_identity->'upgrade_inclusions','[]'::JSONB)
                        || COALESCE(verdict.completed_input_identity->'parents_and_siblings','[]'::JSONB)
                    ) source WHERE COALESCE((source->>'block_height')::BIGINT,(source->>'proof_l1_block_number')::BIGINT,0)>$2
                )))
            ))")
            .bind(first_batch).bind(retained_height).instrument("invalidate_l2_inputs_after_reorg")
            .execute(self.storage).await?;
        Ok(())
    }

    pub async fn supersede_proofs_after_l2_batch(
        &mut self,
        batch: i64,
        cause: i64,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("supersede_proofs_after_l2_batch")
                .constraint_error(error)
        })?;
        sqlx::query(
            "INSERT INTO via_proof_l2_exclusions(excluded_inclusion_id,excluding_inclusion_id)
            SELECT current.id,cause.id FROM via_current_proof_inclusions current
            JOIN via_proof_evidence p ON p.id=current.proof_id
            JOIN via_proof_inclusions cause ON cause.id=$2 AND cause.revoked_at IS NULL
            JOIN via_l1_blocks canonical ON canonical.number=cause.block_height AND canonical.hash=cause.block_hash
            WHERE p.l1_batch_number>$1 AND (current.block_height,current.tx_index,current.message_index)<(cause.block_height,cause.tx_index,cause.message_index)
            ON CONFLICT DO NOTHING",
        )
        .bind(batch)
        .bind(cause)
        .instrument("supersede_proof_membership_after_l2_fork")
        .execute(self.storage)
        .await?;
        Ok(())
    }

    pub async fn get_parent_batch_exists_for_l1_batch(
        &mut self,
        l1_batch_number: i64,
        parent_hash: &[u8],
    ) -> DalResult<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM via_votable_transactions
                WHERE l1_batch_number = $1
                AND prev_l1_batch_hash = $2
            )
            "#,
            l1_batch_number,
            parent_hash
        )
        .instrument("get_parent_batch_exists_for_l1_batch")
        .fetch_one(self.storage)
        .await?;

        Ok(exists.unwrap_or(false))
    }
}

// jscpd:ignore-start Independent database owners retain concrete transactions, role prerequisites and schema migrations.
impl ViaVotesDal<'_, '_> {
    pub async fn record_reader_proof(
        &mut self,
        proof: bitcoin::Txid,
        batch: u32,
        payload: &serde_json::Value,
        inclusion: via_reorg::chain::MessageRef<bitcoin::BlockHash, bitcoin::Txid>,
    ) -> DalResult<i64> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("record_reader_proof")
                .constraint_error(error)
        })?;
        use bitcoin::hashes::Hash;
        use zksync_db_connection::instrument::Instrumented;
        if !self.storage.in_transaction() {
            return Err(Instrumented::new("record_reader_proof")
                .constraint_error(anyhow::anyhow!("proof requires an effects transaction")));
        }
        let (set_id, _) =
            self.storage.via_wallet_dal().verifier_set_before(inclusion.block.height).await?;
        let proof_id: i64 = sqlx::query_scalar("INSERT INTO via_proof_evidence(proof_txid,l1_batch_number,immutable_payload)
            VALUES($1,$2,$3) ON CONFLICT(proof_txid) DO UPDATE
            SET immutable_payload=via_btc_immutable_payload(via_proof_evidence.immutable_payload,EXCLUDED.immutable_payload)
            RETURNING id")
            .bind(proof.as_byte_array().as_slice()).bind(i64::from(batch)).bind(serde_json::json!({"batch":batch,"message":payload}))
            .instrument("reader_proof_evidence").fetch_one(&mut *self.storage).await?;
        sqlx::query_scalar("INSERT INTO via_proof_inclusions(proof_id,carrier_txid,block_hash,block_height,tx_index,message_index,verifier_set_id,l2_eligible)
            VALUES($1,$2,$3,$4,$5,$6,$7,FALSE) ON CONFLICT(proof_id,block_hash,tx_index,message_index) WHERE revoked_at IS NULL DO UPDATE
            SET verifier_set_id=CASE WHEN via_proof_inclusions.verifier_set_id=EXCLUDED.verifier_set_id
                AND via_proof_inclusions.carrier_txid=EXCLUDED.carrier_txid AND via_proof_inclusions.block_height=EXCLUDED.block_height
                THEN via_proof_inclusions.verifier_set_id ELSE NULL END RETURNING id")
            .bind(proof_id).bind(inclusion.carrier_txid.as_byte_array().as_slice())
            .bind(inclusion.block.block_hash.as_byte_array().as_slice()).bind(i64::from(inclusion.block.height))
            .bind(i64::from(inclusion.tx_index)).bind(i64::from(inclusion.message_index)).bind(set_id)
            .instrument("reader_proof_inclusion").fetch_one(self.storage).await
    }

    pub async fn record_reader_vote(
        &mut self,
        identity: bitcoin::Txid,
        proof: bitcoin::Txid,
        verifier: &str,
        approved: bool,
        payload: &serde_json::Value,
        inclusion: via_reorg::chain::MessageRef<bitcoin::BlockHash, bitcoin::Txid>,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("record_reader_vote")
                .constraint_error(error)
        })?;
        use bitcoin::hashes::Hash;
        use zksync_db_connection::instrument::Instrumented;
        if !self.storage.in_transaction() {
            return Err(Instrumented::new("record_reader_vote")
                .constraint_error(anyhow::anyhow!("vote requires an effects transaction")));
        }
        let immutable = serde_json::json!({"proof":proof.to_string(),"verifier":verifier,"approved":approved,"message":payload});
        let vote_id: i64 = sqlx::query_scalar("INSERT INTO via_vote_evidence(txid,message_index,proof_txid,verifier_address,approved,immutable_payload)
            VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(txid,message_index) DO UPDATE
            SET immutable_payload=via_btc_immutable_payload(via_vote_evidence.immutable_payload,EXCLUDED.immutable_payload)
            RETURNING id")
            .bind(identity.as_byte_array().as_slice()).bind(i64::from(inclusion.message_index))
            .bind(proof.as_byte_array().as_slice()).bind(verifier).bind(approved).bind(immutable)
            .instrument("reader_vote_evidence").fetch_one(&mut *self.storage).await?;
        sqlx::query("INSERT INTO via_vote_inclusions(vote_id,carrier_txid,block_hash,block_height,tx_index,message_index)
            VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(vote_id,block_hash,tx_index,message_index) WHERE revoked_at IS NULL DO UPDATE
            SET carrier_txid=CASE WHEN via_vote_inclusions.carrier_txid=EXCLUDED.carrier_txid
                AND via_vote_inclusions.block_height=EXCLUDED.block_height THEN via_vote_inclusions.carrier_txid ELSE NULL END")
            .bind(vote_id).bind(inclusion.carrier_txid.as_byte_array().as_slice())
            .bind(inclusion.block.block_hash.as_byte_array().as_slice()).bind(i64::from(inclusion.block.height))
            .bind(i64::from(inclusion.tx_index)).bind(i64::from(inclusion.message_index))
            .instrument("reader_vote_inclusion").execute(self.storage).await?;
        Ok(())
    }

    /// The exclusive upper coordinate bounds the governing Bitcoin prefix, including within a block.
    pub async fn reader_tally(
        &mut self,
        proof_inclusion: i64,
        before: [i64; 3],
        threshold: f64,
    ) -> DalResult<via_reorg::votes::Tally> {
        use sqlx::Row;
        use zksync_db_connection::instrument::Instrumented;
        let (proof, members): (Vec<u8>,String) = sqlx::query("SELECT p.proof_txid,w.address FROM via_proof_inclusions i
            JOIN via_proof_evidence p ON p.id=i.proof_id JOIN via_wallets w ON w.id=i.verifier_set_id
            JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE i.id=$1 AND i.revoked_at IS NULL AND w.role='Verifier'
                AND (i.block_height,i.tx_index,i.message_index)<($2,$3,$4)")
            .bind(proof_inclusion).bind(before[0]).bind(before[1]).bind(before[2])
            .try_map(|row: sqlx::postgres::PgRow| Ok((row.try_get("proof_txid")?,row.try_get("address")?)))
            .instrument("reader_tally_set").fetch_one(&mut *self.storage).await?;
        let votes = sqlx::query("SELECT e.id,e.proof_txid,e.verifier_address,e.approved,i.block_height,i.tx_index,i.message_index
            FROM via_vote_inclusions i JOIN via_vote_evidence e ON e.id=i.vote_id
            JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE e.proof_txid=$1 AND i.revoked_at IS NULL AND (i.block_height,i.tx_index,i.message_index)<($2,$3,$4)
            ORDER BY i.block_height,i.tx_index,i.message_index")
            .bind(&proof).bind(before[0]).bind(before[1]).bind(before[2])
            .try_map(|row: sqlx::postgres::PgRow| {
                let height: i64 = row.try_get("block_height")?;
                let tx_index: i64 = row.try_get("tx_index")?;
                let message_index: i64 = row.try_get("message_index")?;
                Ok(via_reorg::votes::Vote {
                    identity: row.try_get::<i64,_>("id")?, proof: row.try_get::<Vec<u8>,_>("proof_txid")?,
                    verifier: row.try_get::<String,_>("verifier_address")?, approved: row.try_get("approved")?,
                    position: [height,tx_index,message_index].map(|value| value as u32),
                })
            }).instrument("reader_tally_votes").fetch_all(self.storage).await?;
        let members: Vec<_> = members.split(',').map(str::to_owned).collect();
        via_reorg::votes::tally(&proof, Some(&members), &votes, threshold).map_err(|error| {
            Instrumented::new("reader_tally").constraint_error(anyhow::anyhow!("{error:?}"))
        })
    }

    /// Rotation admission ignores local asynchronous zk-worker completion.
    pub async fn prefix_has_unfinalized_proof(
        &mut self,
        before: [i64; 3],
        threshold: f64,
    ) -> DalResult<bool> {
        let inclusions: Vec<i64> = sqlx::query(
            "SELECT i.id FROM via_proof_inclusions i
            JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE i.revoked_at IS NULL AND (i.block_height,i.tx_index,i.message_index)<($1,$2,$3)",
        )
        .bind(before[0])
        .bind(before[1])
        .bind(before[2])
        .try_map(|row: sqlx::postgres::PgRow| {
            use sqlx::Row;
            row.try_get::<i64, _>("id")
        })
        .instrument("rotation_prefix_proofs")
        .fetch_all(&mut *self.storage)
        .await?;
        for inclusion in inclusions {
            if self.reader_tally(inclusion, before, threshold).await?.finalized.is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn recompute_reader_finalization(
        &mut self,
        through: u32,
        threshold: f64,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("recompute_reader_finalization")
                .constraint_error(error)
        })?;
        use sqlx::Row;
        let inclusions = sqlx::query(
            "SELECT i.id,i.verifier_set_id FROM via_proof_inclusions i
            JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE i.revoked_at IS NULL AND i.block_height<=$1",
        )
        .bind(i64::from(through))
        .try_map(|row: sqlx::postgres::PgRow| {
            Ok((row.try_get::<i64, _>("id")?, row.try_get::<i64, _>("verifier_set_id")?))
        })
        .instrument("reader_finalization_inputs")
        .fetch_all(&mut *self.storage)
        .await?;
        for (id, set_id) in inclusions {
            let verdict: Option<bool> = sqlx::query_scalar(
                "SELECT l1_batch_status FROM via_proof_verdicts WHERE reader_proof_inclusion_id=$1",
            )
            .bind(id)
            .instrument("reader_finalization_verdict")
            .fetch_optional(&mut *self.storage)
            .await?
            .flatten();
            let decision = if verdict.is_some() {
                self.reader_tally(id, [i64::from(through) + 1, 0, 0], threshold).await?.finalized
            } else {
                None
            };
            sqlx::query("INSERT INTO via_proof_finalization(proof_inclusion_id,verifier_set_id,decision)
                VALUES($1,$2,$3) ON CONFLICT(proof_inclusion_id) DO UPDATE SET decision=EXCLUDED.decision,updated_at=NOW() WHERE via_proof_finalization.decision IS DISTINCT FROM EXCLUDED.decision")
                .bind(id).bind(set_id).bind(decision).instrument("recompute_reader_finalization")
                .execute(&mut *self.storage).await?;
        }
        Ok(())
    }

    /// Repeated recovery changes no membership twice and never deletes proof or vote evidence.
    /// Idea: Formance conditional revert at 3ab99b3ad28ffcc6034d94b4e3d831b823cce7a5,
    /// internal/storage/ledger/transactions.go:183-213.
    pub async fn revoke_reader_inclusions_after(&mut self, height: i64) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("revoke_reader_inclusions_after")
                .constraint_error(error)
        })?;
        sqlx::query("UPDATE via_vote_inclusions SET revoked_at=NOW() WHERE block_height>$1 AND revoked_at IS NULL")
            .bind(height).instrument("revoke_reader_votes").execute(&mut *self.storage).await?;
        sqlx::query("UPDATE via_proof_inclusions SET revoked_at=NOW() WHERE block_height>$1 AND revoked_at IS NULL")
            .bind(height).instrument("revoke_reader_proofs").execute(self.storage).await?;
        Ok(())
    }
}
// jscpd:ignore-end
