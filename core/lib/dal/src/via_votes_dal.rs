use via_btc_client::indexer::replay::{Included, ProofInput};
use sqlx::Row;
use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::Core;

// Exact pre-reset outbound receipt identity supplies the main-node Q26 exception.
pub const READER_PROOFS_QUERY: &str = "SELECT p.id,p.payload,h.id AS header_id,h.number AS height,h.hash, TRUE AS local_complete, EXISTS(SELECT 1 FROM via_l1_batch_inscription_request r
               JOIN via_btc_inscriptions_request i ON i.id=r.commit_proof_inscription_id
               JOIN via_btc_inscriptions_request_history sent ON sent.id=i.confirmed_inscriptions_request_history_id
               WHERE r.l1_batch_number=p.l1_batch_number AND r.rotation_legacy_cutoff>=h.number
                 AND sent.reveal_tx_id=p.proof_reveal_tx_id) AS rotation_exempt
             FROM via_btc_proofs p JOIN via_btc_headers h ON h.id=p.proof_header_id AND h.number=p.l1_block_number AND h.hash=p.block_hash
             WHERE h.number<=$1 ORDER BY h.number,p.transaction_index,p.message_index";

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaVotesDal<'_, '_> {
    pub async fn save_reader_proof(
        &mut self,
        proof: &Included<ProofInput>,
        header_id: i64,
    ) -> anyhow::Result<()> {
        use bitcoin::hashes::Hash;
        via_btc_storage::ReaderDal::new(self.storage)
            .check_proof_source(proof, header_id)
            .await?;
        let origin = &proof.origin;
        let raw = origin.txid.to_byte_array();
        let saved = sqlx::query("SELECT p.payload,p.block_hash FROM via_btc_proofs p
            WHERE p.proof_reveal_tx_id=$1 AND (p.block_hash=$2 OR EXISTS(
                SELECT 1 FROM via_btc_headers h WHERE h.number=p.l1_block_number AND h.hash=p.block_hash))")
            .bind(raw.as_slice()).bind(origin.hash.to_string()).instrument("main_proof_replay").fetch_optional(self.storage).await?;
        if let Some(saved) = saved {
            via_btc_client::indexer::replay::compare_fields([(
                "duplicate_canonical_inclusion",
                saved.try_get::<String, _>("block_hash")? == origin.hash.to_string(),
            )])?;
            let old: Included<ProofInput> = serde_json::from_value(saved.try_get("payload")?)?;
            proof.compare(&old)?;
        } else {
            let parent = &proof.payload.input.pubdata;
            let inserted = sqlx::query("INSERT INTO via_btc_proofs(l1_batch_number,proof_reveal_tx_id,pubdata_reveal_tx_id,l1_block_number,block_hash,
                transaction_index,message_index,payload,proof_header_id)
                SELECT $1,$2,$3,h.number,h.hash,$6,$7,$8,h.id FROM via_btc_headers h WHERE h.id=$9 AND h.number=$4 AND h.hash=$5")
                .bind(i64::from(parent.payload.input.l1_batch_index.0)).bind(raw.as_slice()).bind(parent.origin.txid.to_byte_array().as_slice())
                .bind(i64::from(origin.height)).bind(origin.hash.to_string()).bind(i32::try_from(origin.transaction_index)?)
                .bind(i32::try_from(origin.message_index)?).bind(serde_json::to_value(proof)?).bind(header_id)
                .instrument("main_reader_save_proof").execute(self.storage).await?;
            anyhow::ensure!(
                inserted.rows_affected() == 1,
                via_btc_storage::StoreError::HeadChanged
            );
        }
        Ok(())
    }

    /// Inserts a new vote for an l1 batch.
    pub async fn insert_vote(
        &mut self,
        l1_batch_number: u32,
        proof_reveal_tx_id: &[u8],
        verifier_address: &str,
        vote: bool,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
            via_votes (l1_batch_number, proof_reveal_tx_id, verifier_address, vote)
            VALUES
            ($1, $2, $3, $4)
            ON CONFLICT DO NOTHING
            "#,
            l1_batch_number as i32,
            proof_reveal_tx_id,
            verifier_address,
            vote
        )
        .instrument("insert_vote")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(())
    }

    /// Get the voting statistics for a specific L1 batch as (rejections, approvals, total).
    pub async fn get_vote_count(&mut self, l1_batch_number: u32) -> DalResult<(i64, i64, i64)> {
        let row = sqlx::query!(
            r#"
            SELECT
                COUNT(*) FILTER (
                    WHERE
                    vote = FALSE
                ) AS not_ok_votes,
                COUNT(*) FILTER (
                    WHERE
                    vote = TRUE
                ) AS ok_votes,
                COUNT(*) AS total_votes
            FROM
                via_votes
            WHERE
                l1_batch_number = $1
            "#,
            l1_batch_number as i32,
        )
        .instrument("get_vote_count")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        let not_ok_votes = row.not_ok_votes.unwrap_or(0);
        let ok_votes = row.ok_votes.unwrap_or(0);
        let total_votes = row.total_votes.unwrap_or(0);
        Ok((not_ok_votes, ok_votes, total_votes))
    }

    /// Attempts to finalize a batch inscription request based on voting results.
    /// Returns true if the threshold of agreeing votes was reached and the batch was finalized.
    /// Returns false if the batch was already finalized or the threshold hasn't been reached.
    pub async fn finalize_transaction_if_needed(
        &mut self,
        l1_batch_number: u32,
        threshold: f64,
        number_of_verifiers: usize,
    ) -> DalResult<bool> {
        let row = sqlx::query!(
            r#"
            SELECT
                EXISTS(
                    SELECT
                        1
                    FROM
                        via_l1_batch_inscription_request
                    WHERE
                        l1_batch_number = $1
                        AND is_finalized IS NOT NULL
                ) AS already_finalized
            "#,
            i64::from(l1_batch_number),
        )
        .instrument("check_if_already_finalized")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        if row.already_finalized.unwrap() {
            return Ok(false);
        }

        let (not_ok_votes, ok_votes, _total_votes) = self.get_vote_count(l1_batch_number).await?;

        let mut current_threshold = (ok_votes as f64) / (number_of_verifiers as f64);
        let mut is_finalized = true;
        if not_ok_votes > ok_votes {
            current_threshold = (not_ok_votes as f64) / (number_of_verifiers as f64);
            is_finalized = false;
        }
        let reached_threshold = current_threshold >= threshold;

        if !reached_threshold {
            return Ok(false);
        }

        sqlx::query!(
            r#"
            UPDATE via_l1_batch_inscription_request
            SET
                is_finalized = $2,
                updated_at = NOW()
            WHERE
                l1_batch_number = $1
            "#,
            i64::from(l1_batch_number),
            is_finalized
        )
        .instrument("finalize_transaction_if_needed")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(reached_threshold)
    }
}
