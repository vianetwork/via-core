use std::collections::HashSet;

use anyhow::{anyhow, ensure, Context};
use bitcoin::{consensus, hashes::Hash, OutPoint, Transaction, TxOut};
use serde_json::Value;
use sqlx::{postgres::PgRow, Row};
use via_verifier_types::{
    withdrawal::{CompleteWithdrawalBatch, WithdrawalRequest},
    withdrawal_observation::{WithdrawalInclusion, WithdrawalObservation},
};
use zksync_db_connection::{connection::Connection, instrument::InstrumentExt};

use crate::Verifier;

pub struct ViaWithdrawalDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawalAttemptState {
    Admitted,
    MayHaveSigned,
    Signed,
    Finalized,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawalAttemptRecord {
    pub round_id: Vec<u8>,
    pub content: Vec<u8>,
    pub state: WithdrawalAttemptState,
    pub public_nonces: Option<Vec<u8>>,
    pub public_signatures: Option<Vec<u8>>,
    pub finalized_transaction: Option<Vec<u8>>,
}

// Eligibility needs a fresh READ COMMITTED statement after lock acquisition.
// Invalidation takes the exclusive global gate; normal mutations share it and
// take the wallet lock.
async fn lock_wallet(storage: &mut Connection<'_, Verifier>, wallet: &[u8]) -> anyhow::Result<()> {
    ensure!(!wallet.is_empty(), "empty withdrawal wallet context");
    let isolation = sqlx::query("SELECT current_setting('transaction_isolation') AS isolation")
        .map(|row| row)
        .instrument("withdrawal_lock_isolation")
        .fetch_one(&mut *storage)
        .await?;
    ensure!(
        isolation.try_get::<&str, _>("isolation")? == "read committed",
        "withdrawal admission requires READ COMMITTED"
    );
    sqlx::query("SELECT pg_advisory_xact_lock_shared(1464095559)")
        .instrument("withdrawal_invalidation_gate")
        .execute(&mut *storage)
        .await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(encode($1::bytea,'hex'), 1464095560))",
    )
    .bind(wallet)
    .instrument("withdrawal_wallet_lock")
    .execute(&mut *storage)
    .await?;
    sqlx::query("INSERT INTO via_withdrawal_active_wallet(singleton,wallet) VALUES(TRUE,$1) ON CONFLICT DO NOTHING")
        .bind(wallet).instrument("withdrawal_initialize_context").execute(&mut *storage).await?;
    let row = sqlx::query("SELECT wallet FROM via_withdrawal_active_wallet WHERE singleton")
        .map(|row| row)
        .instrument("withdrawal_active_context")
        .fetch_one(&mut *storage)
        .await?;
    ensure!(
        row.try_get::<Vec<u8>, _>("wallet")? == wallet,
        "withdrawal wallet changed: explicit offline reconciliation required"
    );
    Ok(())
}

fn valid_reference(reference: &str) -> bool {
    reference.len() == 20
        && reference
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

async fn hold(
    storage: &mut Connection<'_, Verifier>,
    wallet: &[u8],
    reference: &str,
    reason: &str,
    source: &[u8],
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO via_withdrawal_holds(wallet,reference,reason,source) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(wallet).bind(reference).bind(reason).bind(source)
        .instrument("withdrawal_hold").execute(storage).await?;
    Ok(())
}

async fn conflict(
    storage: &mut Connection<'_, Verifier>,
    wallet: &[u8],
    kind: &str,
    identity: &[u8],
    evidence: Value,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO via_withdrawal_conflicts(wallet,kind,identity,evidence) SELECT $1,$2,$3,$4 WHERE NOT EXISTS (SELECT 1 FROM via_withdrawal_conflicts WHERE wallet=$1 AND kind=$2 AND identity=$3 AND evidence=$4)")
        .bind(wallet).bind(kind).bind(identity).bind(evidence)
        .instrument("withdrawal_conflict_evidence").execute(storage).await?;
    Ok(())
}

async fn revoke_invalid_fulfillments(
    storage: &mut Connection<'_, Verifier>,
    wallet: &[u8],
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE via_withdrawal_fulfillments f SET active=FALSE WHERE f.wallet=$1 AND f.active AND (
         EXISTS(SELECT 1 FROM via_withdrawal_expected e JOIN via_withdrawal_batches b USING(wallet,batch_number) WHERE e.wallet=f.wallet AND e.origin_txid=f.origin_txid AND e.origin_index=f.origin_index AND (b.invalidated OR b.conflicted))
         OR (SELECT count(*) FROM via_withdrawal_origins e WHERE e.wallet=f.wallet AND e.reference=f.reference)<>1
         OR (SELECT count(*) FROM via_withdrawal_outputs o WHERE o.wallet=f.wallet AND o.reference=f.reference)<>1
         OR EXISTS(SELECT 1 FROM via_withdrawal_holds h WHERE h.wallet=f.wallet AND h.reference=f.reference AND h.reason='conflict')
         OR EXISTS(SELECT 1 FROM via_withdrawal_observations o WHERE o.wallet=f.wallet AND o.txid=f.txid AND (o.conflicted OR o.uncertain)))")
        .bind(wallet).instrument("withdrawal_revoke_conflicting_fulfillment").execute(&mut *storage).await?;
    sqlx::query("UPDATE via_withdrawal_observations o SET fulfilled=FALSE WHERE o.wallet=$1 AND o.fulfilled AND EXISTS(SELECT 1 FROM via_withdrawal_outputs p WHERE p.wallet=o.wallet AND p.txid=o.txid AND NOT EXISTS(SELECT 1 FROM via_withdrawal_fulfillments f WHERE f.wallet=p.wallet AND f.txid=p.txid AND f.vout=p.vout AND f.active))")
        .bind(wallet).instrument("withdrawal_reconcile_conflicting_fulfillment").execute(storage).await?;
    Ok(())
}

async fn expected(
    storage: &mut Connection<'_, Verifier>,
    wallet: &[u8],
    references: &[String],
) -> anyhow::Result<Vec<WithdrawalRequest>> {
    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(references.len());
    for reference in references {
        ensure!(
            valid_reference(reference) && seen.insert(reference),
            "duplicate or malformed withdrawal reference"
        );
        let rows = sqlx::query(
            "SELECT e.snapshot, NOT b.invalidated AND NOT b.conflicted
             AND EXISTS(SELECT 1 FROM via_votable_transactions v JOIN via_l1_blocks source ON source.number=v.source_l1_block_number AND source.hash=v.source_l1_block_hash WHERE v.proof_reveal_tx_id=b.proof_txid AND v.l1_batch_number=b.batch_number AND v.pubdata_blob_id=b.blob_id AND v.is_finalized=TRUE AND v.l1_batch_status=TRUE)
             AND (SELECT count(*) FROM via_withdrawal_origins o WHERE o.wallet=e.wallet AND o.reference=e.reference)=1
             AND NOT EXISTS(SELECT 1 FROM via_withdrawal_holds h WHERE h.wallet=e.wallet AND h.reference=e.reference AND h.reason='conflict') AS valid
             FROM via_withdrawal_expected e JOIN via_withdrawal_batches b USING(wallet,batch_number)
             WHERE e.wallet=$1 AND e.reference=$2")
            .bind(wallet).bind(reference).map(|row| row).instrument("withdrawal_expected_snapshot").fetch_all(&mut *storage).await?;
        ensure!(
            rows.len() == 1,
            "missing or ambiguous withdrawal reference {reference}"
        );
        ensure!(
            rows[0].try_get::<bool, _>("valid")?,
            "withdrawal source incomplete, conflicting, or invalidated"
        );
        result.push(serde_json::from_value(
            rows[0].try_get::<Value, _>("snapshot")?,
        )?);
    }
    Ok(result)
}

async fn attempt(
    storage: &mut Connection<'_, Verifier>,
    wallet: &[u8],
    round: &[u8],
) -> anyhow::Result<Option<PgRow>> {
    Ok(
        sqlx::query("SELECT * FROM via_withdrawal_attempts WHERE wallet=$1 AND round_id=$2")
            .bind(wallet)
            .bind(round)
            .instrument("withdrawal_attempt")
            .fetch_optional(storage)
            .await?,
    )
}

fn attempt_record(row: &PgRow) -> anyhow::Result<WithdrawalAttemptRecord> {
    let state = match row.try_get::<&str, _>("state")? {
        "admitted" => WithdrawalAttemptState::Admitted,
        "may_have_signed" => WithdrawalAttemptState::MayHaveSigned,
        "signed" => WithdrawalAttemptState::Signed,
        "finalized" => WithdrawalAttemptState::Finalized,
        "retired" => WithdrawalAttemptState::Retired,
        _ => return Err(anyhow!("unknown persisted withdrawal attempt state")),
    };
    Ok(WithdrawalAttemptRecord {
        round_id: row.try_get("round_id")?,
        content: row.try_get("content")?,
        state,
        public_nonces: row.try_get("public_nonces")?,
        public_signatures: row.try_get("public_signatures")?,
        finalized_transaction: row.try_get("finalized_transaction")?,
    })
}

async fn check_reservations(
    storage: &mut Connection<'_, Verifier>,
    wallet: &[u8],
    requests: &[WithdrawalRequest],
    inputs: &[(OutPoint, TxOut)],
    own_round: Option<&[u8]>,
) -> anyhow::Result<()> {
    let references: Vec<_> = requests.iter().map(|r| r.id.clone()).collect();
    ensure!(
        expected(storage, wallet, &references).await? == requests,
        "stale expected withdrawal snapshot"
    );
    for request in requests {
        let row = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM via_withdrawal_holds WHERE wallet=$1 AND reference=$2)
             OR EXISTS(SELECT 1 FROM via_withdrawal_legacy_holds WHERE reference=$2)
             OR EXISTS(SELECT 1 FROM via_withdrawal_fulfillments WHERE wallet=$1 AND reference=$2)
             OR EXISTS(SELECT 1 FROM via_withdrawal_request_reservations WHERE wallet=$1 AND reference=$2 AND active AND ($3::bytea IS NULL OR round_id<>$3)) AS blocked")
            .bind(wallet).bind(&request.id).bind(own_round).map(|row| row).instrument("withdrawal_request_admission").fetch_one(&mut *storage).await?;
        ensure!(
            !row.try_get::<bool, _>("blocked")?,
            "withdrawal request held or reserved"
        );
    }
    let mut seen = HashSet::new();
    for (outpoint, prevout) in inputs {
        ensure!(
            seen.insert(*outpoint) && prevout.script_pubkey.as_bytes() == wallet,
            "duplicate or foreign bridge input"
        );
        let row = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM via_withdrawal_observed_inputs WHERE wallet=$1 AND prev_txid=$2 AND prev_vout=$3)
             OR EXISTS(SELECT 1 FROM via_withdrawal_input_reservations WHERE wallet=$1 AND prev_txid=$2 AND prev_vout=$3 AND active AND ($4::bytea IS NULL OR round_id<>$4)) AS blocked")
            .bind(wallet).bind(outpoint.txid.as_byte_array().as_slice()).bind(i64::from(outpoint.vout)).bind(own_round).map(|row| row).instrument("withdrawal_input_admission").fetch_one(&mut *storage).await?;
        ensure!(
            !row.try_get::<bool, _>("blocked")?,
            "withdrawal input observed spent or reserved"
        );
    }
    Ok(())
}

impl ViaWithdrawalDal<'_, '_> {
    pub async fn import_complete_batch(
        &mut self,
        wallet: &[u8],
        proof_reveal_tx_id: &[u8],
        batch: &CompleteWithdrawalBatch,
    ) -> anyhow::Result<()> {
        ensure!(
            !self.storage.in_transaction(),
            "complete import requires its own commit boundary"
        );
        ensure!(
            proof_reveal_tx_id.len() == 32,
            "invalid proof transaction identity"
        );
        let evidence = serde_json::to_value(batch)?;
        let mut origins = HashSet::new();
        for (reference, hash, index) in batch
            .withdrawals
            .iter()
            .map(|r| (&r.id, r.l2_tx_hash, r.l2_tx_log_index))
            .chain(
                batch
                    .nonpayable
                    .iter()
                    .map(|r| (&r.id, r.l2_tx_hash, r.l2_tx_log_index)),
            )
        {
            ensure!(valid_reference(reference), "malformed withdrawal reference");
            ensure!(
                origins.insert((hash, index)),
                "duplicate full withdrawal origin"
            );
        }
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let domain = serde_json::json!([batch.chain_id, batch.network, batch.protocol_version]);
        sqlx::query("INSERT INTO via_withdrawal_authority_domain(singleton,domain) VALUES(TRUE,$1) ON CONFLICT DO NOTHING")
            .bind(&domain).instrument("withdrawal_initialize_domain").execute(&mut tx).await?;
        let retained_domain =
            sqlx::query("SELECT domain FROM via_withdrawal_authority_domain WHERE singleton")
                .map(|row| row)
                .instrument("withdrawal_authority_domain")
                .fetch_one(&mut tx)
                .await?;
        ensure!(
            retained_domain.try_get::<Value, _>("domain")? == domain,
            "withdrawal authority chain/network/protocol changed"
        );
        let existing = sqlx::query("SELECT batch_number,proof_txid,evidence FROM via_withdrawal_batches WHERE wallet=$1 AND (batch_number=$2 OR proof_txid=$3)")
            .bind(wallet).bind(i64::from(batch.batch_number)).bind(proof_reveal_tx_id).map(|row| row).instrument("withdrawal_import_replay").fetch_all(&mut tx).await?;
        if !existing.is_empty() {
            if existing.len() == 1
                && existing[0].try_get::<Vec<u8>, _>("proof_txid")? == proof_reveal_tx_id
                && existing[0].try_get::<Value, _>("evidence")? == evidence
            {
                let source = sqlx::query("SELECT EXISTS(SELECT 1 FROM via_votable_transactions v JOIN via_l1_blocks source ON source.number=v.source_l1_block_number AND source.hash=v.source_l1_block_hash WHERE proof_reveal_tx_id=$1 AND l1_batch_number=$2 AND pubdata_blob_id=$3 AND is_finalized=TRUE AND l1_batch_status=TRUE) AS valid")
                    .bind(proof_reveal_tx_id).bind(i64::from(batch.batch_number)).bind(&batch.blob_id).map(|row| row).instrument("withdrawal_revalidate_complete_source").fetch_one(&mut tx).await?;
                ensure!(
                    source.try_get::<bool, _>("valid")?,
                    "withdrawal replay source is not finalized"
                );
                sqlx::query("UPDATE via_withdrawal_batches SET invalidated=FALSE WHERE wallet=$1 AND batch_number=$2 AND NOT conflicted")
                    .bind(wallet).bind(i64::from(batch.batch_number))
                    .instrument("withdrawal_revalidate_equal_batch").execute(&mut tx).await?;
                // Only this exact source's invalidation holds are reversible.
                // Observation, conflict, legacy and signing-risk evidence survives.
                sqlx::query("DELETE FROM via_withdrawal_holds h USING via_withdrawal_expected e,via_withdrawal_batches b WHERE h.wallet=$1 AND h.reason='uncertain' AND h.source=$2 AND e.wallet=h.wallet AND e.reference=h.reference AND e.batch_number=$3 AND b.wallet=e.wallet AND b.batch_number=e.batch_number AND b.proof_txid=h.source AND NOT b.invalidated AND NOT b.conflicted")
                    .bind(wallet).bind(proof_reveal_tx_id).bind(i64::from(batch.batch_number))
                    .instrument("withdrawal_clear_revalidated_source_hold").execute(&mut tx).await?;
                tx.commit().await?;
                return Ok(());
            }
            conflict(&mut tx, wallet, "batch", proof_reveal_tx_id, evidence).await?;
            sqlx::query("UPDATE via_withdrawal_batches SET conflicted=TRUE WHERE wallet=$1 AND (batch_number=$2 OR proof_txid=$3)")
                .bind(wallet).bind(i64::from(batch.batch_number)).bind(proof_reveal_tx_id)
                .instrument("withdrawal_hold_conflicting_batch").execute(&mut tx).await?;
            for reference in batch
                .withdrawals
                .iter()
                .map(|r| &r.id)
                .chain(batch.nonpayable.iter().map(|r| &r.id))
            {
                hold(&mut tx, wallet, reference, "conflict", proof_reveal_tx_id).await?;
            }
            revoke_invalid_fulfillments(&mut tx, wallet).await?;
            tx.commit().await?;
            return Err(anyhow!(
                "conflicting complete withdrawal batch retained and held"
            ));
        }
        let source = sqlx::query("SELECT EXISTS(SELECT 1 FROM via_votable_transactions v JOIN via_l1_blocks source ON source.number=v.source_l1_block_number AND source.hash=v.source_l1_block_hash WHERE proof_reveal_tx_id=$1 AND l1_batch_number=$2 AND pubdata_blob_id=$3 AND is_finalized=TRUE AND l1_batch_status=TRUE) AS valid")
            .bind(proof_reveal_tx_id).bind(i64::from(batch.batch_number)).bind(&batch.blob_id).map(|row| row).instrument("withdrawal_complete_source").fetch_one(&mut tx).await?;
        ensure!(
            source.try_get::<bool, _>("valid")?,
            "withdrawal import source is not finalized"
        );
        sqlx::query("INSERT INTO via_withdrawal_batches(wallet,batch_number,proof_txid,blob_id,evidence) VALUES($1,$2,$3,$4,$5)")
            .bind(wallet).bind(i64::from(batch.batch_number)).bind(proof_reveal_tx_id).bind(&batch.blob_id).bind(evidence)
            .instrument("withdrawal_complete_marker").execute(&mut tx).await?;
        let mut conflicted = false;
        for (reference, hash, index, snapshot, payable) in batch
            .withdrawals
            .iter()
            .map(|r| {
                (
                    &r.id,
                    r.l2_tx_hash,
                    r.l2_tx_log_index,
                    serde_json::to_value(r),
                    Some(r),
                )
            })
            .chain(batch.nonpayable.iter().map(|r| {
                (
                    &r.id,
                    r.l2_tx_hash,
                    r.l2_tx_log_index,
                    serde_json::to_value(r),
                    None,
                )
            }))
        {
            let snapshot = snapshot?;
            let old = sqlx::query("SELECT reference FROM via_withdrawal_origins WHERE wallet=$1 AND origin_txid=$2 AND origin_index=$3")
                .bind(wallet).bind(hash.as_bytes()).bind(i32::from(index))
                .instrument("withdrawal_origin_replay").fetch_optional(&mut tx).await?;
            if let Some(old) = old {
                // One full origin cannot acquire another source-batch membership,
                // even when gross content is identical.
                conflict(&mut tx, wallet, "origin", hash.as_bytes(), snapshot).await?;
                hold(
                    &mut tx,
                    wallet,
                    &old.try_get::<String, _>("reference")?,
                    "conflict",
                    proof_reveal_tx_id,
                )
                .await?;
                hold(&mut tx, wallet, reference, "conflict", proof_reveal_tx_id).await?;
                conflicted = true;
                continue;
            }
            sqlx::query("INSERT INTO via_withdrawal_origins(wallet,origin_txid,origin_index,reference,batch_number) VALUES($1,$2,$3,$4,$5)")
                .bind(wallet).bind(hash.as_bytes()).bind(i32::from(index)).bind(reference).bind(i64::from(batch.batch_number))
                .instrument("withdrawal_immutable_origin").execute(&mut tx).await?;
            if let Some(request) = payable {
                sqlx::query("INSERT INTO via_withdrawal_expected(wallet,origin_txid,origin_index,reference,batch_number,snapshot,gross) VALUES($1,$2,$3,$4,$5,$6,$7::text::numeric)")
                    .bind(wallet).bind(hash.as_bytes()).bind(i32::from(index))
                    .bind(reference).bind(i64::from(batch.batch_number)).bind(snapshot).bind(request.amount.to_sat().to_string())
                    .instrument("withdrawal_immutable_expected").execute(&mut tx).await?;
            }
        }
        if conflicted {
            sqlx::query("UPDATE via_withdrawal_batches SET conflicted=TRUE WHERE wallet=$1 AND batch_number=$2")
                .bind(wallet).bind(i64::from(batch.batch_number)).instrument("withdrawal_origin_conflicted_batch").execute(&mut tx).await?;
        }
        revoke_invalid_fulfillments(&mut tx, wallet).await?;
        tx.commit().await?;
        ensure!(!conflicted, "conflicting full origin retained and held");
        Ok(())
    }

    pub async fn list_eligible_withdrawals(
        &mut self,
        wallet: &[u8],
        min_value: i64,
        limit: u32,
    ) -> anyhow::Result<Vec<WithdrawalRequest>> {
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let rows = sqlx::query(
            "SELECT e.snapshot FROM via_withdrawal_expected e JOIN via_withdrawal_batches b USING(wallet,batch_number)
             WHERE e.wallet=$1 AND e.gross >= $2::bigint AND NOT b.invalidated AND NOT b.conflicted
             AND EXISTS(SELECT 1 FROM via_votable_transactions v JOIN via_l1_blocks source ON source.number=v.source_l1_block_number AND source.hash=v.source_l1_block_hash WHERE v.proof_reveal_tx_id=b.proof_txid AND v.l1_batch_number=b.batch_number AND v.pubdata_blob_id=b.blob_id AND v.is_finalized=TRUE AND v.l1_batch_status=TRUE)
             AND (SELECT count(*) FROM via_withdrawal_origins other WHERE other.wallet=e.wallet AND other.reference=e.reference)=1
             AND NOT EXISTS(SELECT 1 FROM via_withdrawal_legacy_holds h WHERE h.reference=e.reference)
             AND NOT EXISTS(SELECT 1 FROM via_withdrawal_holds h WHERE h.wallet=e.wallet AND h.reference=e.reference)
             AND NOT EXISTS(SELECT 1 FROM via_withdrawal_fulfillments f WHERE f.wallet=e.wallet AND f.reference=e.reference)
             AND NOT EXISTS(SELECT 1 FROM via_withdrawal_request_reservations r WHERE r.wallet=e.wallet AND r.reference=e.reference AND r.active)
             ORDER BY e.batch_number,e.origin_txid,e.origin_index LIMIT $3")
            .bind(wallet).bind(min_value).bind(i64::from(limit)).map(|row| row).instrument("withdrawal_list_eligible").fetch_all(&mut tx).await?;
        let result = rows
            .into_iter()
            .map(|r| Ok(serde_json::from_value(r.try_get::<Value, _>("snapshot")?)?))
            .collect::<anyhow::Result<_>>()?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn load_expected_withdrawals(
        &mut self,
        wallet: &[u8],
        references: &[String],
    ) -> anyhow::Result<Vec<WithdrawalRequest>> {
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let result = expected(&mut tx, wallet, references).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn get_finalized_block_and_non_processed_withdrawal(
        &mut self,
        wallet: &[u8],
        l1_batch_number: i64,
    ) -> anyhow::Result<Option<(String, Vec<u8>)>> {
        let row = sqlx::query("SELECT v.pubdata_blob_id,v.proof_reveal_tx_id FROM via_votable_transactions v JOIN via_l1_blocks source ON source.number=v.source_l1_block_number AND source.hash=v.source_l1_block_hash WHERE v.is_finalized=TRUE AND v.l1_batch_status=TRUE AND v.l1_batch_number=$2 AND NOT EXISTS(SELECT 1 FROM via_withdrawal_batches b WHERE b.wallet=$1 AND b.proof_txid=v.proof_reveal_tx_id AND NOT b.invalidated) ORDER BY v.l1_batch_number LIMIT 1")
            .bind(wallet).bind(l1_batch_number).instrument("get_finalized_block_and_non_processed_withdrawal").fetch_optional(self.storage).await?;
        row.map(|r| {
            Ok((
                r.try_get("pubdata_blob_id")?,
                r.try_get("proof_reveal_tx_id")?,
            ))
        })
        .transpose()
    }

    pub async fn list_finalized_blocks_with_no_bridge_withdrawal(
        &mut self,
        wallet: &[u8],
    ) -> anyhow::Result<Vec<(i64, String, Vec<u8>)>> {
        let rows = sqlx::query("SELECT v.l1_batch_number,v.pubdata_blob_id,v.proof_reveal_tx_id FROM via_votable_transactions v JOIN via_l1_blocks source ON source.number=v.source_l1_block_number AND source.hash=v.source_l1_block_hash WHERE v.is_finalized=TRUE AND v.l1_batch_status=TRUE AND NOT EXISTS(SELECT 1 FROM via_withdrawal_batches b WHERE b.wallet=$1 AND b.proof_txid=v.proof_reveal_tx_id AND NOT b.invalidated) ORDER BY v.l1_batch_number")
            .bind(wallet).map(|row| row).instrument("list_finalized_blocks_with_no_bridge_withdrawal").fetch_all(self.storage).await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.try_get("l1_batch_number")?,
                    r.try_get("pubdata_blob_id")?,
                    r.try_get("proof_reveal_tx_id")?,
                ))
            })
            .collect()
    }
}

fn observation_evidence(observation: &WithdrawalObservation) -> anyhow::Result<Value> {
    let mut immutable = observation.clone();
    immutable.inclusion = None;
    Ok(serde_json::to_value(immutable)?)
}

fn validate_observation(wallet: &[u8], observation: &WithdrawalObservation) -> anyhow::Result<()> {
    ensure!(
        !observation.transaction.input.is_empty(),
        "observation has no inputs"
    );
    ensure!(
        observation.prevouts.len() == observation.transaction.input.len(),
        "incomplete observed prevouts"
    );
    let mut inputs = HashSet::new();
    for (input, (outpoint, _)) in observation
        .transaction
        .input
        .iter()
        .zip(&observation.prevouts)
    {
        ensure!(
            input.previous_output == *outpoint && inputs.insert(*outpoint),
            "wrong or duplicate observed prevout"
        );
    }
    ensure!(
        observation
            .prevouts
            .iter()
            .any(|(_, p)| p.script_pubkey.as_bytes() == wallet),
        "observation has no bridge input"
    );
    let mut outputs = HashSet::new();
    for output in &observation.withdrawals {
        ensure!(
            valid_reference(&output.reference) && outputs.insert(output.vout),
            "malformed or duplicate observed output"
        );
        let actual = observation
            .transaction
            .output
            .get(usize::try_from(output.vout)?)
            .context("observed output out of range")?;
        ensure!(
            actual.script_pubkey == output.script_pubkey && actual.value == output.amount,
            "observed output differs from transaction"
        );
    }
    Ok(())
}

impl ViaWithdrawalDal<'_, '_> {
    pub async fn record_withdrawal_observation(
        &mut self,
        wallet: &[u8],
        observation: &WithdrawalObservation,
    ) -> anyhow::Result<()> {
        ensure!(
            !self.storage.in_transaction(),
            "observation requires its own commit boundary"
        );
        validate_observation(wallet, observation)?;
        let txid = observation.transaction.compute_txid().to_byte_array();
        let evidence = observation_evidence(observation)?;
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let old = sqlx::query("SELECT evidence,accepted_block,accepted_height,uncertain FROM via_withdrawal_observations WHERE wallet=$1 AND txid=$2")
            .bind(wallet).bind(txid.as_slice()).instrument("withdrawal_observation_replay").fetch_optional(&mut tx).await?;
        let differs = old
            .as_ref()
            .map(|r| r.try_get::<Value, _>("evidence"))
            .transpose()?
            .is_some_and(|old| old != evidence);
        if differs {
            conflict(&mut tx, wallet, "observation", &txid, evidence).await?;
            sqlx::query("UPDATE via_withdrawal_observations SET conflicted=TRUE,accepted_block=NULL,accepted_height=NULL,fulfilled=FALSE WHERE wallet=$1 AND txid=$2")
                .bind(wallet).bind(txid.as_slice()).instrument("withdrawal_conflicting_observation").execute(&mut tx).await?;
        } else if old.is_none() {
            sqlx::query(
                "INSERT INTO via_withdrawal_observations(wallet,txid,evidence) VALUES($1,$2,$3)",
            )
            .bind(wallet)
            .bind(txid.as_slice())
            .bind(evidence)
            .instrument("withdrawal_immutable_observation")
            .execute(&mut tx)
            .await?;
            for output in &observation.withdrawals {
                sqlx::query("INSERT INTO via_withdrawal_outputs(wallet,txid,vout,reference,script,amount) VALUES($1,$2,$3,$4,$5,$6::text::numeric)")
                    .bind(wallet).bind(txid.as_slice()).bind(i64::from(output.vout)).bind(&output.reference)
                    .bind(output.script_pubkey.as_bytes()).bind(output.amount.to_sat().to_string())
                    .instrument("withdrawal_actual_output").execute(&mut tx).await?;
            }
        }
        // Holds have no expected-request FK: observed-first and complete-import-first
        // converge, and malformed/conflicting candidate facts never overwrite authority.
        for output in &observation.withdrawals {
            hold(&mut tx, wallet, &output.reference, "observation", &txid).await?;
        }
        for (outpoint, _) in &observation.prevouts {
            sqlx::query("INSERT INTO via_withdrawal_observed_inputs(wallet,txid,prev_txid,prev_vout) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
                .bind(wallet).bind(txid.as_slice()).bind(outpoint.txid.as_byte_array().as_slice()).bind(i64::from(outpoint.vout))
                .instrument("withdrawal_observed_input_hold").execute(&mut tx).await?;
        }
        if let Some(inclusion) = &observation.inclusion {
            sqlx::query("INSERT INTO via_withdrawal_inclusions(wallet,txid,block_hash,block_height,block_hash_text) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
                .bind(wallet).bind(txid.as_slice()).bind(inclusion.block_hash.as_byte_array().as_slice()).bind(i64::from(inclusion.block_height)).bind(inclusion.block_hash.to_string())
                .instrument("withdrawal_inclusion_history").execute(&mut tx).await?;
            let incompatible = if let Some(old) = &old {
                let hash: Option<Vec<u8>> = old.try_get("accepted_block")?;
                let height: Option<i64> = old.try_get("accepted_height")?;
                hash.is_some_and(|hash| hash != inclusion.block_hash.as_byte_array())
                    || height.is_some_and(|height| height != i64::from(inclusion.block_height))
            } else {
                false
            };
            if incompatible {
                sqlx::query("UPDATE via_withdrawal_observations SET uncertain=TRUE,accepted_block=NULL,accepted_height=NULL,fulfilled=FALSE WHERE wallet=$1 AND txid=$2")
                    .bind(wallet).bind(txid.as_slice()).instrument("withdrawal_inclusion_uncertainty").execute(&mut tx).await?;
            } else if !differs {
                sqlx::query("UPDATE via_withdrawal_observations SET accepted_block=$3,accepted_height=$4 WHERE wallet=$1 AND txid=$2 AND NOT uncertain AND NOT conflicted")
                    .bind(wallet).bind(txid.as_slice()).bind(inclusion.block_hash.as_byte_array().as_slice()).bind(i64::from(inclusion.block_height))
                    .instrument("withdrawal_accept_inclusion").execute(&mut tx).await?;
            }
        }
        revoke_invalid_fulfillments(&mut tx, wallet).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn observed_withdrawal_transaction_exists(
        &mut self,
        wallet: &[u8],
        txid: &[u8],
    ) -> anyhow::Result<bool> {
        let row = sqlx::query("SELECT EXISTS(SELECT 1 FROM via_withdrawal_observations WHERE wallet=$1 AND txid=$2) AS present")
            .bind(wallet).bind(txid).map(|row| row).instrument("withdrawal_observation_exists").fetch_one(self.storage).await?;
        Ok(row.try_get("present")?)
    }

    /// Inclusion suppresses identical-byte rebroadcast independently of fulfillment.
    /// Historical or mempool observations alone never establish canonical inclusion.
    pub async fn observed_withdrawal_transaction_has_canonical_inclusion(
        &mut self,
        wallet: &[u8],
        txid: &[u8],
    ) -> anyhow::Result<bool> {
        let row = sqlx::query("SELECT EXISTS(SELECT 1 FROM via_withdrawal_inclusions i JOIN via_l1_blocks l ON l.number=i.block_height AND l.hash=i.block_hash_text WHERE i.wallet=$1 AND i.txid=$2) AS present")
            .bind(wallet).bind(txid).map(|row| row).instrument("withdrawal_observation_canonical_inclusion").fetch_one(self.storage).await?;
        Ok(row.try_get("present")?)
    }

    pub async fn pending_withdrawal_observations(
        &mut self,
        wallet: &[u8],
    ) -> anyhow::Result<Vec<WithdrawalObservation>> {
        let rows = sqlx::query("SELECT o.evidence,c.block_hash AS accepted_block,c.block_height AS accepted_height FROM via_withdrawal_observations o LEFT JOIN LATERAL (SELECT i.block_hash,i.block_height,count(*) OVER() AS candidates FROM via_withdrawal_inclusions i JOIN via_l1_blocks l ON l.number=i.block_height AND l.hash=i.block_hash_text WHERE i.wallet=o.wallet AND i.txid=o.txid) c ON c.candidates=1 WHERE o.wallet=$1 AND NOT o.fulfilled AND NOT o.conflicted ORDER BY o.txid")
            .bind(wallet).map(|row| row).instrument("withdrawal_pending_observations").fetch_all(self.storage).await?;
        rows.into_iter()
            .map(|row| {
                let mut observation: WithdrawalObservation =
                    serde_json::from_value(row.try_get::<Value, _>("evidence")?)?;
                if let (Some(hash), Some(height)) = (
                    row.try_get::<Option<Vec<u8>>, _>("accepted_block")?,
                    row.try_get::<Option<i64>, _>("accepted_height")?,
                ) {
                    observation.inclusion = Some(WithdrawalInclusion {
                        block_hash: bitcoin::BlockHash::from_slice(&hash)?,
                        block_height: u32::try_from(height)?,
                    });
                }
                Ok(observation)
            })
            .collect()
    }

    /// The caller must first validate the complete transaction with the shared
    /// fixed payment verifier. This method accepts no default confirmation policy.
    pub async fn fulfill_withdrawal_observation(
        &mut self,
        wallet: &[u8],
        observation: &WithdrawalObservation,
        requests: &[WithdrawalRequest],
        accepted_tip_height: u32,
        confirmations: u32,
    ) -> anyhow::Result<bool> {
        ensure!(
            confirmations > 0,
            "fulfillment requires explicit positive confirmation policy"
        );
        validate_observation(wallet, observation)?;
        ensure!(
            !requests.is_empty() && requests.len() == observation.withdrawals.len(),
            "incomplete payment membership"
        );
        let Some(inclusion) = &observation.inclusion else {
            return Ok(false);
        };
        if u64::from(accepted_tip_height) + 1
            < u64::from(inclusion.block_height) + u64::from(confirmations)
        {
            return Ok(false);
        }
        let txid = observation.transaction.compute_txid().to_byte_array();
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let canonical = sqlx::query("SELECT EXISTS(SELECT 1 FROM via_l1_blocks WHERE number=$1 AND hash=$2) AND EXISTS(SELECT 1 FROM via_l1_blocks WHERE number=$3) AND EXISTS(SELECT 1 FROM via_withdrawal_inclusions WHERE wallet=$4 AND txid=$5 AND block_height=$1 AND block_hash_text=$2) AND (SELECT count(*) FROM via_withdrawal_inclusions i JOIN via_l1_blocks l ON l.number=i.block_height AND l.hash=i.block_hash_text WHERE i.wallet=$4 AND i.txid=$5)=1 AS valid")
            .bind(i64::from(inclusion.block_height)).bind(inclusion.block_hash.to_string()).bind(i64::from(accepted_tip_height)).bind(wallet).bind(txid.as_slice()).map(|row| row).instrument("withdrawal_canonical_inclusion").fetch_one(&mut tx).await?;
        if !canonical.try_get::<bool, _>("valid")? {
            return Ok(false);
        }
        let Some(stored) = sqlx::query("SELECT evidence,accepted_block,accepted_height,conflicted,uncertain FROM via_withdrawal_observations WHERE wallet=$1 AND txid=$2")
            .bind(wallet).bind(txid.as_slice()).instrument("withdrawal_fulfillment_observation").fetch_optional(&mut tx).await?
            else { return Ok(false); };
        if stored.try_get::<bool, _>("conflicted")?
            || stored.try_get::<Value, _>("evidence")? != observation_evidence(observation)?
        {
            return Ok(false);
        }
        let references: Vec<_> = requests.iter().map(|r| r.id.clone()).collect();
        ensure!(
            expected(&mut tx, wallet, &references).await? == requests,
            "stale fulfillment snapshot"
        );
        let mut seen = HashSet::new();
        for output in &observation.withdrawals {
            ensure!(
                seen.insert(&output.reference),
                "ambiguous payment membership"
            );
            let request = requests
                .iter()
                .find(|r| r.id == output.reference)
                .context("unknown payment member")?;
            ensure!(
                request.receiver.script_pubkey() == output.script_pubkey
                    && output.amount <= request.amount,
                "payment mismatches expected recipient or gross"
            );
            let candidates = sqlx::query(
                "SELECT (SELECT count(*) FROM via_withdrawal_outputs WHERE wallet=$1 AND reference=$2) AS candidates,
                 EXISTS(SELECT 1 FROM via_withdrawal_fulfillments WHERE wallet=$1 AND reference=$2 AND (txid<>$3 OR vout<>$4)) AS other_payment,
                 EXISTS(SELECT 1 FROM via_withdrawal_holds WHERE wallet=$1 AND reference=$2 AND (reason<>'observation' OR source<>$3)) AS other_hold")
                .bind(wallet).bind(&output.reference).bind(txid.as_slice()).bind(i64::from(output.vout)).map(|row| row).instrument("withdrawal_unambiguous_fulfillment").fetch_one(&mut tx).await?;
            if candidates.try_get::<i64, _>("candidates")? != 1
                || candidates.try_get::<bool, _>("other_payment")?
                || candidates.try_get::<bool, _>("other_hold")?
            {
                return Ok(false);
            }
        }
        for output in &observation.withdrawals {
            let request = requests
                .iter()
                .find(|r| r.id == output.reference)
                .context("unknown payment member")?;
            sqlx::query("INSERT INTO via_withdrawal_fulfillments(wallet,reference,txid,vout,accepted_tip,confirmations,origin_txid,origin_index,block_hash,block_height) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT (wallet,origin_txid,origin_index,block_hash,block_height) DO UPDATE SET active=TRUE,accepted_tip=excluded.accepted_tip,confirmations=excluded.confirmations")
                .bind(wallet).bind(&output.reference).bind(txid.as_slice()).bind(i64::from(output.vout))
                .bind(i64::from(accepted_tip_height)).bind(i64::from(confirmations))
                .bind(request.l2_tx_hash.as_bytes()).bind(i32::from(request.l2_tx_log_index))
                .bind(inclusion.block_hash.as_byte_array().as_slice()).bind(i64::from(inclusion.block_height))
                .instrument("withdrawal_fulfill_output").execute(&mut tx).await?;
        }
        sqlx::query("UPDATE via_withdrawal_observations SET fulfilled=TRUE,uncertain=FALSE,accepted_block=$3,accepted_height=$4 WHERE wallet=$1 AND txid=$2")
            .bind(wallet).bind(txid.as_slice()).bind(inclusion.block_hash.as_byte_array().as_slice()).bind(i64::from(inclusion.block_height))
            .instrument("withdrawal_fulfill_observation").execute(&mut tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn invalidate_withdrawals_from_l1_height(
        &mut self,
        height: u32,
    ) -> anyhow::Result<()> {
        self.invalidate(Some(height), None).await
    }

    pub async fn invalidate_withdrawal_batches_from(&mut self, batch: u32) -> anyhow::Result<()> {
        self.invalidate(None, Some(batch)).await
    }

    async fn invalidate(&mut self, height: Option<u32>, batch: Option<u32>) -> anyhow::Result<()> {
        let mut tx = self.storage.start_transaction().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(1464095559)")
            .instrument("withdrawal_exclusive_invalidation")
            .execute(&mut tx)
            .await?;
        // The global gate covers wallets first appearing during invalidation.
        let wallets =
            sqlx::query("SELECT wallet FROM via_withdrawal_active_wallet ORDER BY wallet")
                .map(|row| row)
                .instrument("withdrawal_invalidation_wallets")
                .fetch_all(&mut tx)
                .await?;
        for wallet in wallets {
            let wallet: Vec<u8> = wallet.try_get("wallet")?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(encode($1::bytea,'hex'), 1464095560))")
                .bind(wallet).instrument("withdrawal_invalidation_wallet_lock").execute(&mut tx).await?;
        }
        // None leaves source authority intact for an inclusion-only reorg.
        // An explicit batch zero is a real source boundary and revokes all batches.
        if let Some(batch) = batch {
            sqlx::query(
                "UPDATE via_withdrawal_batches SET invalidated=TRUE WHERE batch_number >= $1",
            )
            .bind(i64::from(batch))
            .instrument("withdrawal_invalidate_batches")
            .execute(&mut tx)
            .await?;
            sqlx::query("INSERT INTO via_withdrawal_holds(wallet,reference,reason,source) SELECT e.wallet,e.reference,'uncertain',b.proof_txid FROM via_withdrawal_expected e JOIN via_withdrawal_batches b USING(wallet,batch_number) WHERE b.invalidated AND b.batch_number >= $1 ON CONFLICT DO NOTHING")
                .bind(i64::from(batch)).instrument("withdrawal_invalidation_holds").execute(&mut tx).await?;
            sqlx::query("UPDATE via_withdrawal_fulfillments f SET active=FALSE FROM via_withdrawal_expected e JOIN via_withdrawal_batches b USING(wallet,batch_number) WHERE f.wallet=e.wallet AND f.origin_txid=e.origin_txid AND f.origin_index=e.origin_index AND b.invalidated")
                .instrument("withdrawal_revoke_source_fulfillment").execute(&mut tx).await?;
            sqlx::query("UPDATE via_withdrawal_observations o SET fulfilled=FALSE WHERE EXISTS(SELECT 1 FROM via_withdrawal_fulfillments f WHERE f.wallet=o.wallet AND f.txid=o.txid AND NOT f.active)")
                .instrument("withdrawal_reconcile_revoked_fulfillment").execute(&mut tx).await?;
        }
        if let Some(height) = height {
            sqlx::query("UPDATE via_withdrawal_observations SET uncertain=TRUE,accepted_block=NULL,accepted_height=NULL,fulfilled=FALSE WHERE accepted_height >= $1")
                .bind(i64::from(height)).instrument("withdrawal_invalidate_inclusions").execute(&mut tx).await?;
            sqlx::query(
                "UPDATE via_withdrawal_fulfillments SET active=FALSE WHERE block_height >= $1",
            )
            .bind(i64::from(height))
            .instrument("withdrawal_revoke_inclusion_fulfillment")
            .execute(&mut tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

impl ViaWithdrawalDal<'_, '_> {
    pub async fn admit_withdrawal_attempt(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
        content: &[u8],
        requests: &[WithdrawalRequest],
        inputs: &[(OutPoint, TxOut)],
    ) -> anyhow::Result<WithdrawalAttemptRecord> {
        self.reserve_withdrawal_attempt(wallet, round_id, content, requests, inputs, false)
            .await
    }

    /// Commit cross-process payment risk before publishing any coordinator proposal.
    /// Exposure does not consume this signer's nonce or advance its signing state.
    pub async fn expose_withdrawal_proposal(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
        content: &[u8],
        requests: &[WithdrawalRequest],
        inputs: &[(OutPoint, TxOut)],
    ) -> anyhow::Result<WithdrawalAttemptRecord> {
        self.reserve_withdrawal_attempt(wallet, round_id, content, requests, inputs, true)
            .await
    }

    async fn reserve_withdrawal_attempt(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
        content: &[u8],
        requests: &[WithdrawalRequest],
        inputs: &[(OutPoint, TxOut)],
        exposed: bool,
    ) -> anyhow::Result<WithdrawalAttemptRecord> {
        ensure!(
            !self.storage.in_transaction(),
            "attempt admission requires its own durable commit"
        );
        ensure!(
            round_id.len() == 32 && !content.is_empty(),
            "invalid withdrawal round identity/content"
        );
        ensure!(
            !requests.is_empty() && !inputs.is_empty(),
            "empty withdrawal attempt"
        );
        let request_json = serde_json::to_value(requests)?;
        let input_json = serde_json::to_value(inputs)?;
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        if let Some(row) = attempt(&mut tx, wallet, round_id).await? {
            ensure!(
                row.try_get::<Vec<u8>, _>("content")? == content
                    && row.try_get::<Value, _>("requests")? == request_json
                    && row.try_get::<Value, _>("inputs")? == input_json,
                "conflicting withdrawal round replay"
            );
            let record = attempt_record(&row)?;
            if exposed {
                ensure!(
                    record.state != WithdrawalAttemptState::Retired,
                    "cannot expose retired withdrawal attempt"
                );
                check_reservations(&mut tx, wallet, requests, inputs, Some(round_id)).await?;
                sqlx::query("UPDATE via_withdrawal_attempts SET coordinator_exposed=TRUE WHERE wallet=$1 AND round_id=$2")
                    .bind(wallet).bind(round_id).instrument("withdrawal_expose_existing_proposal").execute(&mut tx).await?;
            }
            tx.commit().await?;
            return Ok(record);
        }
        check_reservations(&mut tx, wallet, requests, inputs, None).await?;
        sqlx::query("INSERT INTO via_withdrawal_attempts(wallet,round_id,content,requests,inputs,state,coordinator_exposed) VALUES($1,$2,$3,$4,$5,'admitted',$6)")
            .bind(wallet).bind(round_id).bind(content).bind(request_json).bind(input_json).bind(exposed)
            .instrument("withdrawal_admit_attempt").execute(&mut tx).await?;
        for request in requests {
            sqlx::query("INSERT INTO via_withdrawal_request_reservations(wallet,reference,round_id,origin_txid,origin_index) VALUES($1,$2,$3,$4,$5)")
                .bind(wallet).bind(&request.id).bind(round_id)
                .bind(request.l2_tx_hash.as_bytes()).bind(i32::from(request.l2_tx_log_index))
                .instrument("withdrawal_reserve_request").execute(&mut tx).await?;
        }
        for (outpoint, _) in inputs {
            sqlx::query("INSERT INTO via_withdrawal_input_reservations(wallet,prev_txid,prev_vout,round_id) VALUES($1,$2,$3,$4)")
                .bind(wallet).bind(outpoint.txid.as_byte_array().as_slice()).bind(i64::from(outpoint.vout)).bind(round_id)
                .instrument("withdrawal_reserve_input").execute(&mut tx).await?;
        }
        let record = attempt_record(
            &attempt(&mut tx, wallet, round_id)
                .await?
                .context("admitted attempt missing")?,
        )?;
        tx.commit().await?;
        Ok(record)
    }

    /// Selection excludes both active round reservations and every observed spend.
    /// Admission rechecks this snapshot under the wallet lock.
    pub async fn held_withdrawal_inputs(&mut self, wallet: &[u8]) -> anyhow::Result<Vec<OutPoint>> {
        let rows = sqlx::query("SELECT prev_txid,prev_vout FROM via_withdrawal_input_reservations WHERE wallet=$1 AND active UNION SELECT prev_txid,prev_vout FROM via_withdrawal_observed_inputs WHERE wallet=$1")
            .bind(wallet).map(|row| row).instrument("withdrawal_held_inputs").fetch_all(self.storage).await?;
        rows.into_iter()
            .map(|row| {
                Ok(OutPoint {
                    txid: bitcoin::Txid::from_slice(&row.try_get::<Vec<u8>, _>("prev_txid")?)?,
                    vout: u32::try_from(row.try_get::<i64, _>("prev_vout")?)?,
                })
            })
            .collect()
    }

    pub async fn get_withdrawal_attempt(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
    ) -> anyhow::Result<Option<WithdrawalAttemptRecord>> {
        attempt(self.storage, wallet, round_id)
            .await?
            .as_ref()
            .map(attempt_record)
            .transpose()
    }

    pub async fn persist_withdrawal_nonces(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
        content: &[u8],
        nonces: &[u8],
    ) -> anyhow::Result<()> {
        self.persist_public_batch(wallet, round_id, content, nonces, false)
            .await
    }

    pub async fn mark_withdrawal_may_have_signed(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
        content: &[u8],
    ) -> anyhow::Result<()> {
        ensure!(
            !self.storage.in_transaction(),
            "signing risk requires its own durable commit"
        );
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let row = attempt(&mut tx, wallet, round_id)
            .await?
            .context("unknown withdrawal attempt")?;
        let record = attempt_record(&row)?;
        ensure!(
            record.content == content,
            "withdrawal round content changed"
        );
        ensure!(
            record.state != WithdrawalAttemptState::Retired,
            "retired withdrawal attempt"
        );
        if record.state != WithdrawalAttemptState::Admitted {
            tx.commit().await?;
            return Ok(());
        }
        ensure!(
            record.public_nonces.is_some(),
            "complete public nonce batch not durable"
        );
        let requests: Vec<WithdrawalRequest> = serde_json::from_value(row.try_get("requests")?)?;
        let inputs: Vec<(OutPoint, TxOut)> = serde_json::from_value(row.try_get("inputs")?)?;
        check_reservations(&mut tx, wallet, &requests, &inputs, Some(round_id)).await?;
        sqlx::query("UPDATE via_withdrawal_attempts SET state='may_have_signed',signed_risk=TRUE WHERE wallet=$1 AND round_id=$2 AND state='admitted'")
            .bind(wallet).bind(round_id).instrument("withdrawal_durable_signing_risk").execute(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn persist_withdrawal_signatures(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
        content: &[u8],
        signatures: &[u8],
    ) -> anyhow::Result<()> {
        self.persist_public_batch(wallet, round_id, content, signatures, true)
            .await
    }

    async fn persist_public_batch(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
        content: &[u8],
        bytes: &[u8],
        signatures: bool,
    ) -> anyhow::Result<()> {
        ensure!(
            !self.storage.in_transaction(),
            "public results require their own durable commit"
        );
        ensure!(!bytes.is_empty(), "empty public withdrawal batch");
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let row = attempt(&mut tx, wallet, round_id)
            .await?
            .context("unknown withdrawal attempt")?;
        let record = attempt_record(&row)?;
        ensure!(
            record.content == content,
            "withdrawal round content changed"
        );
        let previous = if signatures {
            &record.public_signatures
        } else {
            &record.public_nonces
        };
        if let Some(previous) = previous {
            ensure!(previous == bytes, "conflicting public withdrawal batch");
            tx.commit().await?;
            return Ok(());
        }
        if signatures {
            ensure!(
                record.state == WithdrawalAttemptState::MayHaveSigned
                    && record.public_nonces.is_some(),
                "withdrawal signing risk must be durable first"
            );
            sqlx::query("UPDATE via_withdrawal_attempts SET state='signed',public_signatures=$3 WHERE wallet=$1 AND round_id=$2")
                .bind(wallet).bind(round_id).bind(bytes).instrument("withdrawal_persist_complete_signatures").execute(&mut tx).await?;
        } else {
            ensure!(
                record.state == WithdrawalAttemptState::Admitted,
                "nonce batch cannot change after signing or retirement"
            );
            sqlx::query("UPDATE via_withdrawal_attempts SET public_nonces=$3 WHERE wallet=$1 AND round_id=$2")
                .bind(wallet).bind(round_id).bind(bytes).instrument("withdrawal_persist_complete_nonces").execute(&mut tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn finalize_withdrawal_attempt(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
        content: &[u8],
        transaction: &[u8],
    ) -> anyhow::Result<()> {
        ensure!(
            !self.storage.in_transaction(),
            "finalized bytes require their own durable commit"
        );
        let finalized: Transaction = consensus::deserialize(transaction)?;
        ensure!(
            consensus::serialize(&finalized) == transaction,
            "noncanonical finalized transaction bytes"
        );
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let row = attempt(&mut tx, wallet, round_id)
            .await?
            .context("unknown withdrawal attempt")?;
        let record = attempt_record(&row)?;
        ensure!(
            record.content == content,
            "withdrawal round content changed"
        );
        if let Some(previous) = record.finalized_transaction {
            ensure!(previous == transaction, "conflicting finalized transaction");
            tx.commit().await?;
            return Ok(());
        }
        ensure!(
            record.state == WithdrawalAttemptState::Signed,
            "complete signatures must be durable before finalization"
        );
        let inputs: Vec<(OutPoint, TxOut)> = serde_json::from_value(row.try_get("inputs")?)?;
        ensure!(
            finalized.input.len() == inputs.len()
                && finalized
                    .input
                    .iter()
                    .zip(&inputs)
                    .all(|(input, (outpoint, _))| input.previous_output == *outpoint),
            "finalized transaction has different inputs"
        );
        sqlx::query("UPDATE via_withdrawal_attempts SET state='finalized',finalized_transaction=$3,finalized_txid=$4 WHERE wallet=$1 AND round_id=$2")
            .bind(wallet).bind(round_id).bind(transaction).bind(finalized.compute_txid().as_byte_array().as_slice()).instrument("withdrawal_finalize_before_broadcast").execute(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn retire_withdrawal_attempt(
        &mut self,
        wallet: &[u8],
        round_id: &[u8],
    ) -> anyhow::Result<()> {
        ensure!(
            !self.storage.in_transaction(),
            "attempt retirement requires its own durable commit"
        );
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let row = attempt(&mut tx, wallet, round_id)
            .await?
            .context("unknown withdrawal attempt")?;
        ensure!(
            !matches!(
                attempt_record(&row)?.state,
                WithdrawalAttemptState::Signed | WithdrawalAttemptState::Finalized
            ),
            "signed withdrawal recovery cannot be retired"
        );
        retire(&mut tx, wallet, Some(round_id)).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Requires exclusive ownership of the signer process. Public nonces alone
    /// cannot pay, but the early marker survives every uncertain share-release path.
    pub async fn retire_incomplete_withdrawal_attempts(
        &mut self,
        wallet: &[u8],
    ) -> anyhow::Result<()> {
        ensure!(
            !self.storage.in_transaction(),
            "startup retirement requires its own durable commit"
        );
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        retire(&mut tx, wallet, None).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn verify_withdrawal_wallet(&mut self, wallet: &[u8]) -> anyhow::Result<()> {
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_recoverable_withdrawal_attempts(
        &mut self,
        wallet: &[u8],
    ) -> anyhow::Result<Vec<WithdrawalAttemptRecord>> {
        let rows = sqlx::query("SELECT a.* FROM via_withdrawal_attempts a WHERE a.wallet=$1 AND a.state IN ('signed','finalized') AND NOT EXISTS(SELECT 1 FROM via_withdrawal_inclusions i JOIN via_l1_blocks l ON l.number=i.block_height AND l.hash=i.block_hash_text WHERE i.wallet=a.wallet AND i.txid=a.finalized_txid) ORDER BY a.round_id")
            .bind(wallet).map(|row| row).instrument("withdrawal_recoverable_attempts").fetch_all(self.storage).await?;
        rows.iter().map(attempt_record).collect()
    }

    pub async fn withdrawal_authority_chain_id(&mut self, wallet: &[u8]) -> anyhow::Result<u64> {
        let mut tx = self.storage.start_transaction().await?;
        lock_wallet(&mut tx, wallet).await?;
        let row = sqlx::query("SELECT domain FROM via_withdrawal_authority_domain WHERE singleton")
            .instrument("withdrawal_authority_chain_id")
            .fetch_optional(&mut tx)
            .await?
            .context("no complete withdrawal authority domain")?;
        let domain: Value = row.try_get("domain")?;
        let chain_id = domain
            .get(0)
            .and_then(Value::as_u64)
            .context("invalid retained withdrawal chain ID")?;
        tx.commit().await?;
        Ok(chain_id)
    }

    /// Acquire once on a dedicated, retained connection; never pool this connection
    /// while signing. Health checks do not reacquire a lost lock.
    pub async fn acquire_withdrawal_signer_lock(&mut self, wallet: &[u8]) -> anyhow::Result<bool> {
        ensure!(!wallet.is_empty(), "empty signer wallet");
        if self.check_withdrawal_signer_lock(wallet).await? {
            return Ok(false);
        }
        let row = sqlx::query(
            "SELECT pg_try_advisory_lock(1464095561,hashtext(encode($1::bytea,'hex'))) AS acquired",
        )
        .bind(wallet)
        .map(|row| row)
        .instrument("withdrawal_exclusive_signer_process")
        .fetch_one(self.storage)
        .await?;
        Ok(row.try_get("acquired")?)
    }

    pub async fn check_withdrawal_signer_lock(&mut self, wallet: &[u8]) -> anyhow::Result<bool> {
        let row = sqlx::query("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND pid=pg_backend_pid() AND granted AND classid=1464095561::oid AND objid=((hashtext(encode($1::bytea,'hex'))::bigint & 4294967295)::oid) AND objsubid=2) AS held")
            .bind(wallet).map(|row| row).instrument("withdrawal_signer_process_health").fetch_one(self.storage).await?;
        Ok(row.try_get("held")?)
    }

    pub async fn release_withdrawal_signer_lock(&mut self, wallet: &[u8]) -> anyhow::Result<()> {
        sqlx::query("SELECT pg_advisory_unlock(1464095561,hashtext(encode($1::bytea,'hex')))")
            .bind(wallet)
            .instrument("withdrawal_release_signer_process")
            .execute(self.storage)
            .await?;
        Ok(())
    }
}

async fn retire(
    storage: &mut Connection<'_, Verifier>,
    wallet: &[u8],
    round: Option<&[u8]>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE via_withdrawal_attempts SET state='retired' WHERE wallet=$1 AND state IN ('admitted','may_have_signed') AND ($2::bytea IS NULL OR round_id=$2)")
        .bind(wallet).bind(round).instrument("withdrawal_retire_attempt").execute(&mut *storage).await?;
    sqlx::query("UPDATE via_withdrawal_request_reservations r SET active=FALSE FROM via_withdrawal_attempts a WHERE r.wallet=$1 AND a.wallet=r.wallet AND a.round_id=r.round_id AND a.state='retired' AND NOT a.signed_risk AND NOT a.coordinator_exposed AND ($2::bytea IS NULL OR a.round_id=$2)")
        .bind(wallet).bind(round).instrument("withdrawal_release_unsigned_requests").execute(&mut *storage).await?;
    sqlx::query("UPDATE via_withdrawal_input_reservations r SET active=FALSE FROM via_withdrawal_attempts a WHERE r.wallet=$1 AND a.wallet=r.wallet AND a.round_id=r.round_id AND a.state='retired' AND NOT a.signed_risk AND NOT a.coordinator_exposed AND ($2::bytea IS NULL OR a.round_id=$2)")
        .bind(wallet).bind(round).instrument("withdrawal_release_unsigned_inputs").execute(storage).await?;
    Ok(())
}

#[cfg(test)]
mod tests;
