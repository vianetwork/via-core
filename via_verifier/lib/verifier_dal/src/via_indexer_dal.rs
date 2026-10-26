use anyhow::Context;
use zksync_db_connection::{
    advisory_lock::{AdvisoryLockKey, TransactionAdvisoryLock},
    connection::Connection,
    error::DalResult,
    instrument::InstrumentExt,
};

use crate::{Verifier, VerifierDal, BTC_WATCH_OWNER_LOCK_KEY, WITHDRAWAL_INVALIDATION_LOCK_KEY};

pub const BTC_WATCH_OWNER: TransactionAdvisoryLock = TransactionAdvisoryLock::new(
    AdvisoryLockKey::BigInt(BTC_WATCH_OWNER_LOCK_KEY),
    "btc_watch_owner",
);



#[derive(Debug)]
pub struct ViaIndexerDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Verifier>,
}

impl ViaIndexerDal<'_, '_> {
    // delete_reader_suffix deletes children before parents in one transaction. Completed and local proof parents are retained.
    // Adapted from Era block reverter rollback order: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/block_reverter/src/lib.rs
    pub async fn delete_reader_suffix(&mut self, height: u32) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.storage.in_transaction(),
            "Bitcoin suffix deletion requires a transaction"
        );
        self.lock_btc_watch().await?;
        self.lock_withdrawal_invalidation().await?;
        via_btc_storage::ReaderDal::new(self.storage)
            .check_reset_schema(
                &[
                    "via_transactions",
                    "protocol_versions",
                    "protocol_patches",
                    "via_votable_transactions",
                    "via_votes",
                    "via_wallets",
                    "via_l1_blocks",
                    "via_btc_configuration",
                    "via_btc_headers",
                ],
                &[
                    "protocol_patches.protocol_patches_minor_fkey",
                    "via_l1_batch_vote_inscription_request.via_l1_batch_vote_inscription_reque_votable_transaction_id_fkey",
                    "via_votes.via_votes_votable_transaction_id_fkey",
                    "via_btc_consumers.via_btc_consumers_header_id_fkey",
                    "via_btc_consumers.via_btc_consumers_oldest_needed_id_fkey",
                ],
            )
            .await?;
        self.storage
            .via_withdrawal_dal()
            .invalidate_withdrawals_from_l1_height(
                height.checked_add(1).context("Bitcoin suffix height overflow")?,
            )
            .await?;
        via_btc_storage::ReaderDal::new(self.storage).delete_common_suffix(height).await?;
        // Suffix deletion keeps protocol history, which retained work needs.
        // Votable rows carry no protocol-version link to delete by.
        // Idea only from Era block reverter, which also keeps protocol versions on rollback: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/block_reverter/src/lib.rs
        for query in [
            "DELETE FROM via_votable_transactions p WHERE (p.proof_l1_block_number>$1 OR p.proof_l1_block_number IS NULL)
                AND p.l1_batch_status IS NULL
                AND NOT EXISTS(SELECT 1 FROM via_votes v WHERE v.votable_transaction_id=p.id)
                AND NOT EXISTS(SELECT 1 FROM via_l1_batch_vote_inscription_request r WHERE r.votable_transaction_id=p.id)
                AND NOT EXISTS(SELECT 1 FROM via_l1_batch_bridge_withdrawals b WHERE b.proof_reveal_tx_id=p.proof_reveal_tx_id)
                AND NOT EXISTS(SELECT 1 FROM via_withdrawal_batches b WHERE b.proof_txid=p.proof_reveal_tx_id)",
            "DELETE FROM via_transactions WHERE COALESCE(bitcoin_height,l1_block_number)>$1
                AND status IS NULL AND l1_batch_number IS NULL",
        ] {
            sqlx::query(query).bind(i64::from(height)).instrument("verifier_bitcoin_delete_suffix")
                .execute(self.storage).await?;
        }
        Ok(())
    }


    /// Lets only one verifier scan, header update, initialization or recovery run at a time.
    /// Call it inside the transaction that commits the work, before reading progress or fetching blocks.
    /// Work that can invalidate withdrawals must then call `lock_withdrawal_invalidation` before any wallet lock.
    // Graph-node's `rewind_or_truncate_with_conn` saves block pointer and indexed data in one transaction.
    // Via does same and holds lock while fetching too, because several tasks share one checkpoint:
    // https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs
    pub async fn lock_btc_watch(&mut self) -> DalResult<()> {
        BTC_WATCH_OWNER.acquire(self.storage).await
    }

    /// Blocks withdrawal work and proof or vote writes while this transaction may invalidate withdrawals.
    /// Take the scan lock first, then this lock, then any wallet lock.
    /// In any other order, two operations can wait for each other forever.
    pub async fn lock_withdrawal_invalidation(&mut self) -> DalResult<()> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(WITHDRAWAL_INVALIDATION_LOCK_KEY)
            .instrument("btc_watch_invalidation_gate")
            .execute(&mut *self.storage)
            .await?;
        Ok(())
    }
}

impl via_btc_storage::ReaderRole for Verifier {
    const NAME: &'static str = "verifier";
    const OWNER: TransactionAdvisoryLock = BTC_WATCH_OWNER;
    const REQUIRED_CONSUMERS: &'static [&'static str] =
        &[via_btc_storage::PROTOCOL_CONSUMER, "withdrawals"];

    async fn authorize_publication(
        storage: &mut Connection<'_, Self>,
        message: &via_btc_client::types::InscriptionMessage,
        voter: &bitcoin::Address,
    ) -> anyhow::Result<()> {
        via_btc_storage::ReaderDal::new(storage).authorize_vote_message(message, voter).await
    }
    async fn save_publication(
        storage: &mut Connection<'_, Self>,
        request_id: i64,
        p: via_btc_storage::publication::Publication,
    ) -> anyhow::Result<()> {
        storage
            .via_btc_sender_dal()
            .insert_inscription_request_history(
                p.commit_txid.to_string(),
                p.reveal_txid.to_string(),
                request_id,
                p.signed_commit,
                p.signed_reveal,
                p.fees,
                p.sent_at,
            )
            .await?;
        Ok(())
    }
    async fn save_proof(
        storage: &mut Connection<'_, Self>,
        proof: &via_btc_client::indexer::replay::Included<
            via_btc_client::indexer::replay::ProofInput,
        >,
        header_id: i64,
    ) -> anyhow::Result<()> {
        storage.via_votes_dal().save_reader_proof(proof, header_id).await
    }
    async fn initialize_bootstrap(
        storage: &mut Connection<'_, Self>,
        bootstrap: &via_btc_client::bootstrap::CheckedBootstrap,
    ) -> anyhow::Result<()> {
        storage.via_protocol_versions_dal().bind_reader_bootstrap(bootstrap).await
    }

    async fn lock_source(storage: &mut Connection<'_, Self>) -> anyhow::Result<()> {
        storage.via_indexer_dal().lock_withdrawal_invalidation().await?;
        Ok(())
    }
    async fn lock_authority_source(storage: &mut Connection<'_, Self>) -> anyhow::Result<()> {
        sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
            .bind(WITHDRAWAL_INVALIDATION_LOCK_KEY)
            .instrument("bitcoin_authority_source_gate")
            .execute(storage)
            .await?;
        Ok(())
    }
    async fn delete_suffix(storage: &mut Connection<'_, Self>, height: u32) -> anyhow::Result<()> {
        storage.via_indexer_dal().delete_reader_suffix(height).await
    }
    async fn materialize_protocol(
        storage: &mut Connection<'_, Self>,
        change: &via_btc_storage::StoredConfiguration,
    ) -> anyhow::Result<()> {
        storage.via_protocol_versions_dal().bind_reader_protocol(change).await
    }
    async fn apply_protocol_block(
        storage: &mut Connection<'_, Self>,
        block: &via_btc_storage::protocol::ProtocolBlock,
    ) -> anyhow::Result<()> {
        for deposit in &block.deposits {
            let operation = &deposit.operation;
            let transaction = crate::via_transactions_dal::L1ToL2Transaction {
                priority_id: operation.common_data.serial_id.0 as i64,
                l1_block_number: i64::from(deposit.origin.height),
                tx_id: deposit.txid,
                receiver: format!("{:#x}", deposit.payload.input.0.receiver_l2_address),
                value: deposit.payload.input.1 as i64,
                calldata: deposit.payload.input.0.call_data.clone(),
                canonical_tx_hash: operation.common_data.canonical_tx_hash,
            };
            storage
                .via_transactions_dal()
                .insert_transaction(transaction, &deposit.origin, &deposit.payload)
                .await?;
        }
        Ok(())
    }
    async fn stamp_legacy_cutoff(
        storage: &mut Connection<'_, Self>,
        cutoff: u32,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE via_votable_transactions SET rotation_legacy_cutoff=$1
    WHERE is_finalized IS NOT NULL AND rotation_legacy_cutoff IS NULL",
        )
        .bind(i64::from(cutoff))
        .instrument("bitcoin_legacy_rotation_cutoff")
        .execute(storage)
        .await?;
        Ok(())
    }

    async fn unreconciled_source(
        storage: &mut Connection<'_, Self>,
    ) -> anyhow::Result<Option<String>> {
        sqlx::query_scalar::<_, String>("(SELECT 'deposit:' || encode(t.canonical_tx_hash,'hex') FROM via_transactions t
            WHERE t.bitcoin_payload IS NULL OR NOT EXISTS(SELECT 1 FROM via_btc_headers h
                WHERE h.number=t.bitcoin_height AND h.hash=t.bitcoin_hash) LIMIT 1)
            UNION ALL (SELECT 'votable:' || p.id::TEXT FROM via_votable_transactions p WHERE p.bitcoin_payload IS NULL
                OR NOT EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.id=p.proof_header_id AND h.hash=p.proof_l1_block_hash) LIMIT 1)
            UNION ALL (SELECT 'protocol_minor:' || v.id::TEXT FROM protocol_versions v WHERE v.executed AND (
                NOT EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.number=v.bitcoin_height AND h.hash=v.bitcoin_hash)
                OR NOT EXISTS(SELECT 1 FROM protocol_patches p JOIN via_btc_headers h
                    ON h.number=p.bitcoin_height AND h.hash=p.bitcoin_hash WHERE p.minor=v.id)) LIMIT 1) LIMIT 1")
            .instrument("bitcoin_reader_retained_reconciliation").fetch_optional(storage).await.map_err(Into::into)
    }

    async fn retained_boundary(storage: &mut Connection<'_, Self>) -> anyhow::Result<Option<u32>> {
        let height = sqlx::query_scalar::<_, Option<i64>>("SELECT max(height) FROM (
            SELECT h.number AS height FROM via_transactions t JOIN via_btc_headers h ON h.number=t.bitcoin_height AND h.hash=t.bitcoin_hash
                WHERE t.status IS NOT NULL OR t.l1_batch_number IS NOT NULL
            UNION ALL SELECT h.number FROM protocol_versions v JOIN via_btc_headers h ON h.number=v.bitcoin_height AND h.hash=v.bitcoin_hash WHERE v.executed
            UNION ALL SELECT h.number FROM protocol_patches p JOIN protocol_versions v ON v.id=p.minor
                JOIN via_btc_headers h ON h.number=p.bitcoin_height AND h.hash=p.bitcoin_hash WHERE v.executed
            UNION ALL SELECT h.number FROM via_votable_transactions p JOIN via_btc_headers h ON h.id=p.proof_header_id
                WHERE p.l1_batch_status IS NOT NULL
                   OR EXISTS(SELECT 1 FROM via_l1_batch_vote_inscription_request r WHERE r.votable_transaction_id=p.id)
                   OR EXISTS(SELECT 1 FROM via_withdrawal_batches w WHERE w.proof_txid=p.proof_reveal_tx_id)
            UNION ALL SELECT h.number FROM via_votes v JOIN via_votable_transactions p ON p.id=v.votable_transaction_id
                JOIN via_btc_headers h ON h.number=v.source_l1_block_number AND h.hash=v.source_l1_block_hash
                WHERE h.number<=p.rotation_legacy_cutoff
            ) retained")
            .instrument("bitcoin_retained_authority_boundary").fetch_one(storage).await?;
        height.map(u32::try_from).transpose().map_err(Into::into)
    }

    async fn retained_proof_txids(
        storage: &mut Connection<'_, Self>,
    ) -> anyhow::Result<Vec<bitcoin::Txid>> {
        use bitcoin::hashes::Hash;
        let rows = sqlx::query_as::<_, (Vec<u8>,)>("SELECT DISTINCT p.proof_reveal_tx_id FROM via_votable_transactions p
    WHERE (p.l1_batch_status IS NOT NULL
        OR EXISTS(SELECT 1 FROM via_l1_batch_vote_inscription_request r WHERE r.votable_transaction_id=p.id)
        OR EXISTS(SELECT 1 FROM via_l1_batch_bridge_withdrawals w WHERE w.proof_reveal_tx_id=p.proof_reveal_tx_id)
        OR EXISTS(SELECT 1 FROM via_withdrawal_batches w WHERE w.proof_txid=p.proof_reveal_tx_id))
    AND NOT EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.id=p.proof_header_id AND h.hash=p.proof_l1_block_hash)")
            .instrument("bitcoin_retained_proof_sources")
            .fetch_all(storage)
            .await?;
        rows.into_iter()
            .map(|(mut bytes,)| {
                bytes.reverse();
                bitcoin::Txid::from_slice(&bytes).map_err(Into::into)
            })
            .collect()
    }

    async fn canonical_proofs(
        storage: &mut Connection<'_, Self>,
        through: u32,
        scope: via_btc_storage::ProofScope,
    ) -> anyhow::Result<Vec<via_btc_storage::CanonicalProof>> {
        let txid = match scope {
            via_btc_storage::ProofScope::Publication(txid) => {
                Some(via_btc_client::indexer::replay::transaction_hash(txid).as_bytes().to_vec())
            }
            _ => None,
        };
        let rows = sqlx::query("SELECT p.id,p.bitcoin_payload AS payload,h.id AS header_id,h.number AS height,h.hash,
                p.l1_batch_status IS NOT NULL AND p.id>m.designated_after_votable_id
                  AND (NOT p.unverified_dev OR m.proof_verification_dev_mode) AS local_complete,
                COALESCE(p.rotation_legacy_cutoff>=h.number,FALSE) AS rotation_exempt
             FROM via_votable_transactions p JOIN via_btc_headers h
               ON h.id=p.proof_header_id AND h.number=p.proof_l1_block_number AND h.hash=p.proof_l1_block_hash
             CROSS JOIN via_verifier_store_mode m WHERE h.number<=$1 AND ($3::BYTEA IS NULL OR p.proof_reveal_tx_id=$3) AND p.bitcoin_payload IS NOT NULL AND (NOT $2 OR p.finalized_header_id IS DISTINCT FROM p.proof_header_id)
             ORDER BY h.number,p.transaction_index,p.message_index")
            .bind(i64::from(through)).bind(matches!(scope, via_btc_storage::ProofScope::Finalization)).bind(txid)
            .map(|row: sqlx::postgres::PgRow| row)
            .instrument("bitcoin_canonical_proofs")
            .fetch_all(storage)
            .await?;
        rows.iter().map(via_btc_storage::CanonicalProof::from_row).collect()
    }

    async fn finalize_proof(
        storage: &mut Connection<'_, Self>,
        work: &via_btc_storage::ProofWork,
        approved: bool,
    ) -> anyhow::Result<u64> {
        Ok(sqlx::query("UPDATE via_votable_transactions p SET is_finalized=$3,finalized_header_id=$2,updated_at=NOW()
    FROM via_btc_headers h WHERE p.id=$1 AND p.proof_header_id=$2 AND (p.is_finalized IS NULL OR p.finalized_header_id IS DISTINCT FROM $2)
      AND h.id=p.proof_header_id AND h.number=p.proof_l1_block_number AND h.hash=p.proof_l1_block_hash")
            .bind(work.id)
            .bind(work.header_id)
            .bind(approved)
            .instrument("bitcoin_finalize_exact_proof")
            .execute(storage)
            .await?
            .rows_affected())
    }
}
