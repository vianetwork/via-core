use via_btc_client::indexer::replay::{Included, ProofInput};
use bitcoin::BlockHash;
use sqlx::Row;
use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::H256;

use crate::{
    models::storage_vote::CanonicalChainStatus, Verifier, WITHDRAWAL_INVALIDATION_LOCK_KEY,
};

// The immutable pre-reset finalization marker supplies the verifier Q26 exception.

// Verifier proof IDs are display-order H256 bytes; reverse at the Bitcoin Txid boundary.

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

// Acquire this inside a real transaction, before reading accepted local chain
// facts. The reverter holds the exclusive counterpart through source deletion.
async fn lock_source(
    storage: &mut Connection<'_, Verifier>,
    inclusion: Option<(u32, BlockHash)>,
) -> DalResult<()> {
    let isolation = sqlx::query("SELECT current_setting('transaction_isolation') AS isolation")
        .map(|row: sqlx::postgres::PgRow| row.get::<String, _>("isolation"))
        .instrument("source_transaction_isolation")
        .fetch_one(&mut *storage)
        .await?;
    if isolation != "read committed" {
        return Err(Instrumented::new("source_transaction_isolation")
            .constraint_error(anyhow::anyhow!("source writes require READ COMMITTED")));
    }
    sqlx::query(if inclusion.is_some() {
        "SELECT pg_advisory_xact_lock_shared($1)"
    } else {
        // Losing provenance invalidates authority and must exclude signing.
        "SELECT pg_advisory_xact_lock($1)"
    })
    .bind(WITHDRAWAL_INVALIDATION_LOCK_KEY)
    .instrument("source_invalidation_gate")
    .execute(&mut *storage)
    .await?;
    if let Some((height, hash)) = inclusion {
        let current = sqlx::query("SELECT 1 FROM via_btc_headers WHERE number=$1 AND hash=$2")
            .bind(i64::from(height))
            .bind(hash.to_string())
            .instrument("source_canonical_inclusion")
            .fetch_optional(storage)
            .await?;
        if current.is_none() {
            return Err(Instrumented::new("source_canonical_inclusion")
                .with_arg("height", &height)
                .with_arg("hash", &hash)
                .constraint_error(anyhow::anyhow!(
                    "source inclusion is not on the accepted local Bitcoin chain"
                )));
        }
    }
    Ok(())
}

impl ViaVotesDal<'_, '_> {
    pub async fn pending_reader_vote(&mut self) -> anyhow::Result<Option<(i64, bool, Vec<u8>)>> {
        let row = sqlx::query_as::<_, (i64, bool, Vec<u8>)>("SELECT p.id,p.l1_batch_status,p.proof_reveal_tx_id
            FROM via_votable_transactions p JOIN via_btc_headers h ON h.id=p.proof_header_id
              AND h.number=p.proof_l1_block_number AND h.hash=p.proof_l1_block_hash
            CROSS JOIN via_btc_reader r CROSS JOIN via_verifier_store_mode m
            WHERE r.replay_ready AND r.stopped_reason IS NULL AND h.number<=r.confirmed_number
              AND p.id>m.designated_after_votable_id AND (NOT p.unverified_dev OR m.proof_verification_dev_mode)
              AND p.l1_batch_status IS NOT NULL AND p.bitcoin_payload IS NOT NULL
              AND NOT EXISTS(SELECT 1 FROM via_l1_batch_vote_inscription_request request WHERE request.votable_transaction_id=p.id)
            ORDER BY h.number,p.transaction_index,p.message_index LIMIT 1")
            .instrument("pending_canonical_reader_vote").fetch_all(self.storage).await?;
        Ok(row.into_iter().next())
    }

    pub async fn pending_reader_proof(
        &mut self,
    ) -> anyhow::Result<Option<via_btc_storage::ProofWork>> {
        let row = sqlx::query("SELECT p.id,p.bitcoin_payload AS payload,h.id AS header_id,h.number AS height,h.hash
            FROM via_votable_transactions p JOIN via_btc_headers h ON h.id=p.proof_header_id
              AND h.number=p.proof_l1_block_number AND h.hash=p.proof_l1_block_hash
            WHERE p.l1_batch_status IS NULL AND p.bitcoin_payload IS NOT NULL AND (p.l1_batch_number=1
              OR EXISTS(SELECT 1 FROM via_votable_transactions parent JOIN via_btc_headers b ON b.id=parent.proof_header_id
                AND b.number=parent.proof_l1_block_number AND b.hash=parent.proof_l1_block_hash
                WHERE parent.l1_batch_status=TRUE AND parent.l1_batch_number=p.l1_batch_number-1
                  AND parent.l1_batch_hash=p.prev_l1_batch_hash))
            ORDER BY p.l1_batch_number,h.number,p.transaction_index,p.message_index LIMIT 1")
            .instrument("pending_reader_proof").fetch_optional(self.storage).await?;
        row.as_ref().map(via_btc_storage::ProofWork::from_row).transpose()
    }

    pub async fn complete_reader_proof(
        &mut self,
        work: &via_btc_storage::ProofWork,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query("UPDATE via_votable_transactions p SET l1_batch_status=TRUE,updated_at=NOW()
            FROM via_btc_headers h WHERE p.id=$1 AND p.proof_header_id=$2 AND h.id=$2 AND h.number=$3 AND h.hash=$4
              AND p.proof_l1_block_number=h.number AND p.proof_l1_block_hash=h.hash AND p.l1_batch_status IS NULL")
            .bind(work.id).bind(work.header_id).bind(i64::from(work.proof.origin.height)).bind(work.proof.origin.hash.to_string())
            .instrument("complete_reader_proof").execute(self.storage).await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn save_reader_proof(
        &mut self,
        proof: &Included<ProofInput>,
        header_id: i64,
    ) -> anyhow::Result<()> {
        use via_btc_client::indexer::replay::transaction_hash;
        via_btc_storage::ReaderDal::new(self.storage).check_proof_source(proof, header_id).await?;
        let input = &proof.payload.input;
        let pubdata = &input.pubdata.payload.input;
        let proof_txid = transaction_hash(proof.origin.txid);
        let proof_id = self
            .insert_votable_transaction(
                pubdata.l1_batch_index.0,
                pubdata.l1_batch_hash,
                pubdata.prev_l1_batch_hash,
                input.proof.da_identifier.clone(),
                proof_txid,
                input.proof.blob_id.clone(),
                input.proof.l1_batch_reveal_txid.to_string(),
                pubdata.blob_id.clone(),
                Some((proof.origin.height, proof.origin.hash)),
            )
            .await?;
        let payload = sqlx::query_scalar::<_, Option<serde_json::Value>>(
            "SELECT bitcoin_payload FROM via_votable_transactions WHERE id=$1 FOR UPDATE",
        )
        .bind(proof_id)
        .instrument("verifier_proof_binding")
        .fetch_one(self.storage)
        .await?;
        if let Some(payload) = payload {
            let mut old: Included<ProofInput> = serde_json::from_value(payload)?;
            old.origin = via_btc_client::indexer::replay::MessageOrigin {
                txid: old.origin.txid,
                ..proof.origin.clone()
            };
            old.payload.input.pubdata.origin = via_btc_client::indexer::replay::MessageOrigin {
                txid: old.payload.input.pubdata.origin.txid,
                ..proof.payload.input.pubdata.origin.clone()
            };
            proof.compare(&old)?;
        }
        let result = sqlx::query("UPDATE via_votable_transactions p SET bitcoin_payload=$2,proof_header_id=h.id,
            proof_l1_block_number=h.number,proof_l1_block_hash=h.hash,source_l1_block_number=h.number,source_l1_block_hash=h.hash,
            transaction_index=$3,message_index=$4 FROM via_btc_headers h
            WHERE p.id=$1 AND h.id=$5 AND h.number=$6 AND h.hash=$7")
            .bind(proof_id).bind(serde_json::to_value(proof)?)
            .bind(i32::try_from(proof.origin.transaction_index)?).bind(i32::try_from(proof.origin.message_index)?)
            .bind(header_id).bind(i64::from(proof.origin.height)).bind(proof.origin.hash.to_string())
            .instrument("verifier_bind_proof_source").execute(self.storage).await?;
        anyhow::ensure!(result.rows_affected() == 1, via_btc_storage::StoreError::HeadChanged);
        sqlx::query("WITH RECURSIVE ancestors AS (
            SELECT id,l1_batch_number,prev_l1_batch_hash,l1_batch_status FROM via_votable_transactions WHERE id=$1
            UNION ALL SELECT p.id,p.l1_batch_number,p.prev_l1_batch_hash,p.l1_batch_status FROM via_votable_transactions p
            JOIN ancestors child ON p.l1_batch_number=child.l1_batch_number-1 AND p.l1_batch_hash=child.prev_l1_batch_hash
            JOIN via_btc_headers h ON h.id=p.proof_header_id AND h.number=p.proof_l1_block_number AND h.hash=p.proof_l1_block_hash)
            UPDATE via_votable_transactions p SET l1_batch_status=FALSE,is_finalized=FALSE
            WHERE p.id=$1 AND p.l1_batch_status IS NULL AND EXISTS(SELECT 1 FROM ancestors WHERE id<>$1 AND l1_batch_status=FALSE)")
            .bind(proof_id).instrument("reader_inherit_rejected_ancestor").execute(self.storage).await?;
        Ok(())
    }

    /// Inserts a new row in `via_votable_transactions`.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_votable_transaction(
        &mut self,
        l1_batch_number: u32,
        l1_batch_hash: H256,
        prev_l1_batch_hash: H256,
        da_identifier: String,
        proof_reveal_tx_id: H256,
        proof_blob_id: String,
        pubdata_reveal_tx_id: String,
        pubdata_blob_id: String,
        source_inclusion: Option<(u32, BlockHash)>,
    ) -> anyhow::Result<i64> {
        let mut tx = self.storage.start_transaction().await?;
        lock_source(&mut tx, source_inclusion).await?;
        let source_hash = source_inclusion.map(|(_, hash)| hash.to_string());
        let old = sqlx::query(
            "SELECT p.*, EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.number=p.proof_l1_block_number AND h.hash=p.proof_l1_block_hash) AS canonical
             FROM via_votable_transactions p WHERE p.proof_reveal_tx_id=$1
             ORDER BY (p.proof_l1_block_hash IS NOT DISTINCT FROM $2) DESC,canonical DESC,p.id LIMIT 1 FOR UPDATE",
        )
        .bind(proof_reveal_tx_id.as_bytes())
        .bind(&source_hash)
        .instrument("proof_payload_replay")
        .fetch_optional(&mut tx)
        .await?;
        if let Some(old) = old {
            let hash: Vec<u8> = old.try_get("l1_batch_hash")?;
            let parent: Vec<u8> = old.try_get("prev_l1_batch_hash")?;
            let saved = (
                old.try_get::<i64, _>("l1_batch_number")?,
                hash.as_slice(),
                parent.as_slice(),
                old.try_get::<&str, _>("da_identifier")?,
                old.try_get::<&str, _>("proof_blob_id")?,
                old.try_get::<&str, _>("pubdata_reveal_tx_id")?,
                old.try_get::<&str, _>("pubdata_blob_id")?,
            );
            let incoming = (
                i64::from(l1_batch_number),
                l1_batch_hash.as_bytes(),
                prev_l1_batch_hash.as_bytes(),
                da_identifier.as_str(),
                proof_blob_id.as_str(),
                pubdata_reveal_tx_id.as_str(),
                pubdata_blob_id.as_str(),
            );
            via_btc_client::indexer::replay::compare_fields([
                ("l1_batch_number", incoming.0 == saved.0),
                ("l1_batch_hash", incoming.1 == saved.1),
                ("prev_l1_batch_hash", incoming.2 == saved.2),
                ("da_identifier", incoming.3 == saved.3),
                ("proof_blob_id", incoming.4 == saved.4),
                ("pubdata_reveal_tx_id", incoming.5 == saved.5),
                ("pubdata_blob_id", incoming.6 == saved.6),
            ])?;
            if let (Some(saved), Some(incoming)) =
                (old.try_get::<Option<String>, _>("proof_l1_block_hash")?, source_hash)
            {
                anyhow::ensure!(
                    saved == incoming || !old.try_get::<bool, _>("canonical")?,
                    via_btc_client::indexer::replay::ReplayConflict {
                        field: "duplicate_canonical_inclusion"
                    }
                );
            }
            let id = old.try_get("id")?;
            tx.commit().await?;
            return Ok(id);
        }
        let id = sqlx::query_scalar::<_,i64>(
            "INSERT INTO via_votable_transactions (
                l1_batch_number,l1_batch_hash,prev_l1_batch_hash,proof_reveal_tx_id,
                da_identifier,proof_blob_id,pubdata_reveal_tx_id,pubdata_blob_id,
                source_l1_block_number,source_l1_block_hash,
                proof_l1_block_number,proof_l1_block_hash
             ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$9,$10) RETURNING id",
        )
        .bind(i64::from(l1_batch_number))
        .bind(l1_batch_hash.as_bytes())
        .bind(prev_l1_batch_hash.as_bytes())
        .bind(proof_reveal_tx_id.as_bytes())
        .bind(da_identifier)
        .bind(proof_blob_id)
        .bind(pubdata_reveal_tx_id)
        .bind(pubdata_blob_id)
        .bind(source_inclusion.map(|(height, _)| i64::from(height)))
        .bind(source_inclusion.map(|(_, hash)| hash.to_string()))
        .instrument("insert_votable_transaction")
        .fetch_one(&mut tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn get_votable_transaction_id(
        &mut self,
        proof_reveal_tx_id: &[u8],
    ) -> DalResult<Option<i64>> {
        let row = sqlx::query!(
            r#"
            SELECT
                id
            FROM
                via_votable_transactions
            WHERE
                proof_reveal_tx_id = $1
            "#,
            proof_reveal_tx_id,
        )
        .instrument("get_votable_transaction_id")
        .fetch_optional(self.storage)
        .await?;
        Ok(row.map(|r| r.id))
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

    pub async fn verify_votable_transaction(
        &mut self,
        l1_batch_number: i64,
        proof_reveal_tx_id: H256,
        l1_batch_status: bool,
    ) -> DalResult<i64> {
        let record = sqlx::query!(
            r#"
            UPDATE via_votable_transactions
            SET
                l1_batch_status = $3,
                updated_at = NOW()
            WHERE
                l1_batch_number = $1
                AND proof_reveal_tx_id = $2
            RETURNING
            id
            "#,
            l1_batch_number,
            proof_reveal_tx_id.as_bytes(),
            l1_batch_status
        )
        .instrument("verify_votable_transaction")
        .fetch_one(self.storage)
        .await?;

        // Invalidate all subsequent batches if an invalid L1 batch is detected.
        if !l1_batch_status {
            sqlx::query!(
                r#"
                UPDATE via_votable_transactions
                SET
                    l1_batch_status = FALSE,
                    is_finalized = FALSE,
                    updated_at = NOW()
                WHERE
                    l1_batch_number > $1
                "#,
                l1_batch_number
            )
            .instrument("verify_votable_transaction")
            .execute(self.storage)
            .await?;
        }

        Ok(record.id)
    }

    /// Records that development mode accepted this result without a proof.
    /// It fails on a store not designated for development, which rolls back the enclosing verdict.
    /// RISC Zero similarly re-checks dev mode where a fake receipt is accepted, not only at startup:
    /// https://github.com/risc0/risc0/blob/218e3bc4a8ffcd203a9cd4e46f921bf60aa7e2bd/risc0/zkvm/src/receipt.rs#L443-L445
    pub async fn mark_unverified_dev(&mut self, votable_transaction_id: i64) -> anyhow::Result<()> {
        let updated = sqlx::query(
            "UPDATE via_votable_transactions SET unverified_dev = TRUE WHERE id = $1 \
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
        let row = sqlx::query!(
            r#"
            WITH
            last_verified AS (
                SELECT
                    l1_batch_number,
                    l1_batch_hash
                FROM
                    via_votable_transactions
                WHERE
                    l1_batch_status = TRUE
                ORDER BY
                    l1_batch_number DESC
                LIMIT
                    1
            )
            
            SELECT
                v.l1_batch_number,
                v.proof_reveal_tx_id
            FROM
                via_votable_transactions v
            LEFT JOIN last_verified lv ON v.prev_l1_batch_hash = lv.l1_batch_hash
            WHERE
                v.l1_batch_status IS NULL
                AND (
                    v.l1_batch_number = 1
                    OR (
                        lv.l1_batch_hash IS NOT NULL
                        AND v.l1_batch_number = lv.l1_batch_number + 1
                    )
                )
            ORDER BY
                v.l1_batch_number ASC
            LIMIT
                1
            "#,
        )
        .instrument("get_first_not_verified_l1_batch_in_canonical_inscription_chain")
        .fetch_optional(self.storage)
        .await?;

        let result = row.map(|r| {
            let l1_batch_number = r.l1_batch_number;
            let proof_reveal_tx_id = r.proof_reveal_tx_id;
            (l1_batch_number, proof_reveal_tx_id)
        });

        Ok(result)
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

    pub async fn delete_votable_transactions(&mut self, l1_batch_number: i64) -> DalResult<()> {
        sqlx::query_scalar!(
            r#"
            DELETE FROM via_votable_transactions 
            WHERE l1_batch_number > $1
            "#,
            l1_batch_number
        )
        .instrument("delete_votable_transactions")
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
