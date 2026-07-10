//! Postgres implementation of the ingestion kernel's [`AggregateAdapter`].
//!
//! One adapter serves all three node databases. The shadow schema is the
//! same per family; only the role's projection set and downstream boundary
//! differ. Each block commits in one transaction guarded by the checkpoint
//! row. Reverts restore from the chain table; raw bytes and inclusion
//! history are never deleted.

use async_trait::async_trait;
use bitcoin::{hashes::Hash, Amount, BlockHash, OutPoint, ScriptBuf, Txid, Wtxid};
use sqlx::{PgPool, Postgres, Transaction as PgTx};
use via_btc_ingestion::{
    AggregateAdapter, ApplyError, BlockAnchor, BlockPlan, CanonicalObservedTx, Checkpoint, CoverageReport, EffectId,
    EventKind, HardReorgHalt, InfraError, KernelVersion, ObservationReadError, ObservationReader,
    ObservationRuleVersion, ProjectionReceipt, ProtocolContext, ProtocolEvent, RawTxVariant, RevertError,
    RevertReceipt, Role, TrackedOutputCreate, TrackedRole,
};

/// Stages where tests may inject faults. Without a hook the checks cost
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    AfterRawVariants,
    AfterInclusions,
    AfterTrackedOutputs,
    AfterDomainProjections,
    AfterProtocolContext,
    AfterCheckpointUpdate,
    BeforeCommit,
    AfterCommitBeforeAck,
    RevertAfterCanonicalUnwind,
    RevertAfterProjectionUnwind,
    RevertAfterContextRestore,
    RevertAfterCheckpointRestore,
    RevertBeforeCommit,
    RevertAfterCommitBeforeAck,
}

/// Test-only fault hook: return true to trip at this stage. The closure
/// itself is where a harness records which stages were reached.
pub type FaultHook = std::sync::Arc<dyn Fn(Stage) -> bool + Send + Sync>;

pub struct PgIngestionAdapter {
    pool: PgPool,
    role: Role,
    fault: Option<FaultHook>,
}

/// Event kinds this role projects into fact tables. The rest are reported
/// as irrelevant in the receipt.
fn projected(role: Role, kind: EventKind) -> bool {
    use EventKind::*;
    match role {
        Role::CoreSequencer => !matches!(kind, BridgeWithdrawal),
        Role::Verifier => true,
        Role::StandaloneIndexer => {
            matches!(kind, Deposit | BridgeWithdrawal | SystemBootstrapping | UpdateBridgeProposal | WalletRotation)
        }
    }
}

fn infra(e: impl std::fmt::Display) -> InfraError {
    InfraError(e.to_string())
}

fn effect_key(effect: &EffectId) -> Vec<u8> {
    serde_json::to_vec(effect).expect("EffectId serializes")
}

impl PgIngestionAdapter {
    pub fn new(pool: PgPool, role: Role) -> Self {
        Self { pool, role, fault: None }
    }

    pub fn with_fault_hook(pool: PgPool, role: Role, fault: FaultHook) -> Self {
        Self { pool, role, fault: Some(fault) }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn trip(&self, stage: Stage) -> bool {
        self.fault.as_ref().is_some_and(|hook| hook(stage))
    }

    #[allow(clippy::type_complexity)]
    async fn load_checkpoint_tx(
        &self, tx: &mut PgTx<'_, Postgres>,
    ) -> Result<Option<(Checkpoint, ProtocolContext)>, InfraError> {
        let row: Option<(i64, Vec<u8>, i32, i32, Vec<u8>, Vec<u8>, serde_json::Value)> = sqlx::query_as(
            "SELECT height, block_hash, kernel_version, observation_rule_version, \
                 context_hash, last_plan_hash, context_blob \
                 FROM via_ingestion_checkpoint WHERE id FOR UPDATE",
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(infra)?;
        row.map(|(height, hash, kv, orv, ctx_hash, plan_hash, blob)| {
            let context: ProtocolContext = serde_json::from_value(blob).map_err(infra)?;
            Ok((
                Checkpoint {
                    height: height as u64,
                    hash: block_hash_from(&hash)?,
                    kernel_version: KernelVersion(kv as u32),
                    observation_rule_version: ObservationRuleVersion(orv as u32),
                    context_hash: hash32_from(&ctx_hash)?,
                    last_plan_hash: hash32_from(&plan_hash)?,
                },
                context,
            ))
        })
        .transpose()
    }
}

fn hash32_from(bytes: &[u8]) -> Result<[u8; 32], InfraError> {
    bytes.try_into().map_err(|_| infra("stored hash is not 32 bytes"))
}

fn block_hash_from(bytes: &[u8]) -> Result<BlockHash, InfraError> {
    Ok(BlockHash::from_byte_array(hash32_from(bytes)?))
}

#[async_trait]
impl AggregateAdapter for PgIngestionAdapter {
    async fn load_checkpoint_and_context(&self) -> Result<Option<(Checkpoint, ProtocolContext)>, InfraError> {
        let mut tx = self.pool.begin().await.map_err(infra)?;
        let out = self.load_checkpoint_tx(&mut tx).await?;
        tx.commit().await.map_err(infra)?;
        Ok(out)
    }

    async fn apply_block(
        &self, expected_checkpoint: Option<Checkpoint>, plan: &BlockPlan,
    ) -> Result<ProjectionReceipt, ApplyError> {
        plan.validate()?;
        let plan_hash = plan.plan_hash()?;
        let mut tx = self.pool.begin().await.map_err(infra)?;

        let halted: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT halt_blob FROM via_ingestion_hard_reorg_halt WHERE id")
                .fetch_optional(&mut *tx)
                .await
                .map_err(infra)?;
        if let Some((blob,)) = halted {
            let halt: HardReorgHalt = serde_json::from_value(blob).map_err(infra)?;
            return Err(ApplyError::Halted(halt));
        }

        let stored = self.load_checkpoint_tx(&mut tx).await?;
        let stored_cp = stored.as_ref().map(|(cp, _)| *cp);
        if stored_cp != expected_checkpoint {
            return Err(ApplyError::StaleCheckpoint {
                expected: Box::new(expected_checkpoint),
                actual: Box::new(stored_cp),
            });
        }
        if let Some(cp) = &stored_cp {
            if cp.kernel_version != plan.kernel_version || cp.observation_rule_version != plan.observation_rule_version
            {
                return Err(ApplyError::VersionMismatch(format!(
                    "checkpoint {:?}/{:?}, plan {:?}/{:?}",
                    cp.kernel_version, cp.observation_rule_version, plan.kernel_version, plan.observation_rule_version
                )));
            }
            if plan.anchor.height != cp.height + 1 || plan.anchor.prev_hash != cp.hash {
                return Err(ApplyError::NotAdjacent {
                    plan_height: plan.anchor.height,
                    plan_prev: plan.anchor.prev_hash,
                    checkpoint_height: cp.height,
                    checkpoint_hash: cp.hash,
                });
            }
            if plan.input_context_hash != cp.context_hash {
                return Err(ApplyError::ContextMismatch {
                    plan_context: plan.input_context_hash,
                    checkpoint_context: cp.context_hash,
                });
            }
        }

        let block = plan.anchor.hash.as_byte_array().to_vec();
        for v in &plan.raw_variants {
            sqlx::query(
                "INSERT INTO via_ingestion_raw_variants (txid, wtxid, raw) VALUES ($1, $2, $3) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(v.txid().as_byte_array().to_vec())
            .bind(v.wtxid().as_byte_array().to_vec())
            .bind(v.raw().to_vec())
            .execute(&mut *tx)
            .await
            .map_err(infra)?;
        }
        if self.trip(Stage::AfterRawVariants) {
            return Err(InfraError("injected fault: after raw variants".into()).into());
        }

        for i in &plan.inclusions {
            sqlx::query(
                "INSERT INTO via_ingestion_inclusions \
                 (block_hash, height, tx_index, txid, wtxid, canonical) \
                 VALUES ($1, $2, $3, $4, $5, TRUE) \
                 ON CONFLICT (block_hash, tx_index) DO UPDATE SET canonical = TRUE",
            )
            .bind(&block)
            .bind(i.height as i64)
            .bind(i.tx_index as i64)
            .bind(i.txid.as_byte_array().to_vec())
            .bind(i.wtxid.as_byte_array().to_vec())
            .execute(&mut *tx)
            .await
            .map_err(infra)?;
        }
        if self.trip(Stage::AfterInclusions) {
            return Err(InfraError("injected fault: after inclusions".into()).into());
        }

        for c in &plan.tracked_creates {
            sqlx::query(
                "INSERT INTO via_ingestion_tracked_outputs \
                 (txid, vout, value_sat, script, role, canonical_spender_txid, created_block_hash) \
                 VALUES ($1, $2, $3, $4, $5, NULL, $6) ON CONFLICT (txid, vout) DO NOTHING",
            )
            .bind(c.outpoint.txid.as_byte_array().to_vec())
            .bind(c.outpoint.vout as i64)
            .bind(c.value.to_sat() as i64)
            .bind(c.script_pubkey.as_bytes().to_vec())
            .bind(c.role.wire_tag() as i16)
            .bind(&block)
            .execute(&mut *tx)
            .await
            .map_err(infra)?;
        }
        for s in &plan.tracked_spends {
            sqlx::query(
                "INSERT INTO via_ingestion_tracked_spends \
                 (txid, vout, spending_txid, spending_wtxid, input_index, block_hash) \
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
            )
            .bind(s.outpoint.txid.as_byte_array().to_vec())
            .bind(s.outpoint.vout as i64)
            .bind(s.spending_txid.as_byte_array().to_vec())
            .bind(s.spending_wtxid.as_byte_array().to_vec())
            .bind(s.input_index as i64)
            .bind(&block)
            .execute(&mut *tx)
            .await
            .map_err(infra)?;
            sqlx::query(
                "UPDATE via_ingestion_tracked_outputs SET canonical_spender_txid = $3 \
                 WHERE txid = $1 AND vout = $2",
            )
            .bind(s.outpoint.txid.as_byte_array().to_vec())
            .bind(s.outpoint.vout as i64)
            .bind(s.spending_txid.as_byte_array().to_vec())
            .execute(&mut *tx)
            .await
            .map_err(infra)?;
        }
        if self.trip(Stage::AfterTrackedOutputs) {
            return Err(InfraError("injected fault: after tracked outputs".into()).into());
        }

        let mut applied = Vec::new();
        let mut irrelevant = Vec::new();
        for event in &plan.events {
            let effect = event.effect_id(plan.anchor.hash);
            if !projected(self.role, event.kind()) {
                irrelevant.push(effect);
                continue;
            }
            applied.push(effect);
            project_event(&mut tx, &block, plan, event).await.map_err(ApplyError::from)?;
        }
        for d in &plan.dispositions {
            if let via_btc_ingestion::DispositionKind::RejectedInvalid { code, .. } = &d.kind {
                sqlx::query(
                    "INSERT INTO via_ingestion_rejections \
                     (block_hash, ordinal_tx_index, ordinal_location_tag, ordinal_location_index, code) \
                     VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                )
                .bind(&block)
                .bind(d.ordinal.tx_index as i64)
                .bind(d.ordinal.location.wire_tag() as i16)
                .bind(d.ordinal.location.index() as i64)
                .bind(code.wire_tag() as i16)
                .execute(&mut *tx)
                .await
                .map_err(infra)?;
            }
        }
        if self.trip(Stage::AfterDomainProjections) {
            return Err(InfraError("injected fault: after projections".into()).into());
        }

        let context_blob = serde_json::to_value(&plan.next_context).map_err(infra)?;
        sqlx::query(
            "INSERT INTO via_ingestion_chain \
             (height, block_hash, prev_hash, header_time, plan_hash, context_blob) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(plan.anchor.height as i64)
        .bind(&block)
        .bind(plan.anchor.prev_hash.as_byte_array().to_vec())
        .bind(plan.anchor.time as i64)
        .bind(plan_hash.to_vec())
        .bind(&context_blob)
        .execute(&mut *tx)
        .await
        .map_err(infra)?;
        if self.trip(Stage::AfterProtocolContext) {
            return Err(InfraError("injected fault: after protocol context".into()).into());
        }

        sqlx::query(
            "INSERT INTO via_ingestion_checkpoint \
             (id, height, block_hash, kernel_version, observation_rule_version, \
              context_hash, last_plan_hash, context_blob, canonical_revision) \
             VALUES (TRUE, $1, $2, $3, $4, $5, $6, $7, 0) \
             ON CONFLICT (id) DO UPDATE SET height = $1, block_hash = $2, \
              kernel_version = $3, observation_rule_version = $4, context_hash = $5, \
              last_plan_hash = $6, context_blob = $7, updated_at = now()",
        )
        .bind(plan.anchor.height as i64)
        .bind(&block)
        .bind(plan.kernel_version.0 as i32)
        .bind(plan.observation_rule_version.0 as i32)
        .bind(plan.next_context.context_hash().to_vec())
        .bind(plan_hash.to_vec())
        .bind(&context_blob)
        .execute(&mut *tx)
        .await
        .map_err(infra)?;
        if self.trip(Stage::AfterCheckpointUpdate) {
            return Err(InfraError("injected fault: after checkpoint update".into()).into());
        }
        if self.trip(Stage::BeforeCommit) {
            return Err(InfraError("injected fault: before commit".into()).into());
        }

        let ack_lost = self.trip(Stage::AfterCommitBeforeAck);
        tx.commit().await.map_err(infra)?;
        if ack_lost {
            return Err(ApplyError::CommitIndeterminate { height: plan.anchor.height, plan_hash });
        }

        Ok(ProjectionReceipt { role: self.role, plan_hash, applied, irrelevant_to_role: irrelevant })
    }

    async fn revert_to(
        &self, expected_checkpoint: Checkpoint, ancestor: BlockAnchor,
    ) -> Result<RevertReceipt, RevertError> {
        let mut tx = self.pool.begin().await.map_err(infra)?;
        let stored = self.load_checkpoint_tx(&mut tx).await?;
        let stored_cp = stored.as_ref().map(|(cp, _)| *cp);
        if stored_cp != Some(expected_checkpoint) {
            return Err(RevertError::StaleCheckpoint {
                expected: Box::new(expected_checkpoint),
                actual: Box::new(stored_cp),
            });
        }
        let (revision,): (i64,) = sqlx::query_as("SELECT canonical_revision FROM via_ingestion_checkpoint WHERE id")
            .fetch_one(&mut *tx)
            .await
            .map_err(infra)?;

        if ancestor.hash == expected_checkpoint.hash {
            tx.commit().await.map_err(infra)?;
            return Ok(RevertReceipt { reverted_to: ancestor, canonical_revision: revision as u64 });
        }
        let ancestor_row: Option<(Vec<u8>, serde_json::Value, Vec<u8>)> =
            sqlx::query_as("SELECT block_hash, context_blob, plan_hash FROM via_ingestion_chain WHERE height = $1")
                .bind(ancestor.height as i64)
                .fetch_optional(&mut *tx)
                .await
                .map_err(infra)?;
        let Some((anc_hash, anc_context, anc_plan_hash)) = ancestor_row else {
            return Err(RevertError::UnknownAncestor(ancestor.hash));
        };
        if block_hash_from(&anc_hash)? != ancestor.hash {
            return Err(RevertError::UnknownAncestor(ancestor.hash));
        }

        sqlx::query(
            "UPDATE via_ingestion_inclusions SET canonical = FALSE WHERE block_hash IN \
             (SELECT block_hash FROM via_ingestion_chain WHERE height > $1)",
        )
        .bind(ancestor.height as i64)
        .execute(&mut *tx)
        .await
        .map_err(infra)?;
        sqlx::query(
            "UPDATE via_ingestion_tracked_outputs o SET canonical_spender_txid = NULL \
             WHERE canonical_spender_txid IS NOT NULL AND EXISTS (\
                SELECT 1 FROM via_ingestion_tracked_spends s \
                JOIN via_ingestion_chain c ON c.block_hash = s.block_hash \
                WHERE s.txid = o.txid AND s.vout = o.vout \
                  AND s.spending_txid = o.canonical_spender_txid AND c.height > $1)",
        )
        .bind(ancestor.height as i64)
        .execute(&mut *tx)
        .await
        .map_err(infra)?;
        sqlx::query(
            "DELETE FROM via_ingestion_tracked_outputs WHERE created_block_hash IN \
             (SELECT block_hash FROM via_ingestion_chain WHERE height > $1)",
        )
        .bind(ancestor.height as i64)
        .execute(&mut *tx)
        .await
        .map_err(infra)?;
        if self.trip(Stage::RevertAfterCanonicalUnwind) {
            return Err(InfraError("injected fault: revert canonical unwind".into()).into());
        }

        for table in [
            "via_ingestion_deposits",
            "via_ingestion_batch_refs",
            "via_ingestion_proof_refs",
            "via_ingestion_votes",
            "via_ingestion_withdrawals",
            "via_ingestion_wallet_history",
            "via_ingestion_protocol_versions",
            "via_ingestion_rejections",
        ] {
            sqlx::query(&format!(
                "DELETE FROM {table} WHERE block_hash IN \
                 (SELECT block_hash FROM via_ingestion_chain WHERE height > $1)"
            ))
            .bind(ancestor.height as i64)
            .execute(&mut *tx)
            .await
            .map_err(infra)?;
        }
        if self.trip(Stage::RevertAfterProjectionUnwind) {
            return Err(InfraError("injected fault: revert projection unwind".into()).into());
        }

        if self.trip(Stage::RevertAfterContextRestore) {
            return Err(InfraError("injected fault: revert context restore".into()).into());
        }
        sqlx::query("DELETE FROM via_ingestion_chain WHERE height > $1")
            .bind(ancestor.height as i64)
            .execute(&mut *tx)
            .await
            .map_err(infra)?;

        let anc_ctx: ProtocolContext = serde_json::from_value(anc_context).map_err(infra)?;
        sqlx::query(
            "UPDATE via_ingestion_checkpoint SET height = $1, block_hash = $2, \
             context_hash = $3, last_plan_hash = $4, context_blob = $5, \
             canonical_revision = canonical_revision + 1, updated_at = now() WHERE id",
        )
        .bind(ancestor.height as i64)
        .bind(anc_hash)
        .bind(anc_ctx.context_hash().to_vec())
        .bind(anc_plan_hash)
        .bind(serde_json::to_value(&anc_ctx).map_err(infra)?)
        .execute(&mut *tx)
        .await
        .map_err(infra)?;
        if self.trip(Stage::RevertAfterCheckpointRestore) {
            return Err(InfraError("injected fault: revert checkpoint restore".into()).into());
        }
        if self.trip(Stage::RevertBeforeCommit) {
            return Err(InfraError("injected fault: revert before commit".into()).into());
        }
        let ack_lost = self.trip(Stage::RevertAfterCommitBeforeAck);
        tx.commit().await.map_err(infra)?;
        if ack_lost {
            return Err(RevertError::CommitIndeterminate);
        }
        Ok(RevertReceipt { reverted_to: ancestor, canonical_revision: (revision + 1) as u64 })
    }

    async fn downstream_consumed(&self, effects: &[EffectId]) -> Result<Vec<EffectId>, InfraError> {
        if self.role == Role::StandaloneIndexer {
            return Ok(vec![]);
        }
        let mut consumed = Vec::new();
        for effect in effects {
            let row: Option<(Vec<u8>,)> =
                sqlx::query_as("SELECT effect_key FROM via_ingestion_consumed_effects WHERE effect_key = $1")
                    .bind(effect_key(effect))
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(infra)?;
            if row.is_some() {
                consumed.push(*effect);
            }
        }
        Ok(consumed)
    }

    async fn record_hard_reorg_halt(&self, halt: HardReorgHalt) -> Result<(), InfraError> {
        sqlx::query(
            "INSERT INTO via_ingestion_hard_reorg_halt (id, halt_blob) VALUES (TRUE, $1) \
             ON CONFLICT (id) DO UPDATE SET halt_blob = $1",
        )
        .bind(serde_json::to_value(&halt).map_err(infra)?)
        .execute(&self.pool)
        .await
        .map_err(infra)?;
        Ok(())
    }

    async fn hard_reorg_halt(&self) -> Result<Option<HardReorgHalt>, InfraError> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT halt_blob FROM via_ingestion_hard_reorg_halt WHERE id")
                .fetch_optional(&self.pool)
                .await
                .map_err(infra)?;
        row.map(|(blob,)| serde_json::from_value(blob).map_err(infra)).transpose()
    }

    async fn audit_coverage(&self, from_height: u64, to_height: u64) -> Result<CoverageReport, InfraError> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM via_ingestion_chain WHERE height BETWEEN $1 AND $2")
                .bind(from_height as i64)
                .bind(to_height as i64)
                .fetch_one(&self.pool)
                .await
                .map_err(infra)?;
        let expected = to_height.saturating_sub(from_height) + 1;
        Ok(CoverageReport {
            from_height,
            to_height,
            contiguous: count as u64 == expected,
            unresolved_dependencies: vec![],
        })
    }
}

async fn project_event(
    tx: &mut PgTx<'_, Postgres>, block: &[u8], plan: &BlockPlan, event: &ProtocolEvent,
) -> Result<(), InfraError> {
    match event {
        ProtocolEvent::DepositObserved(d) => {
            let source_wtxid = plan
                .inclusions
                .iter()
                .find(|i| i.txid == d.subject.txid)
                .map(|i| i.wtxid)
                .ok_or_else(|| infra("deposit subject has no inclusion"))?;
            sqlx::query(
                "INSERT INTO via_ingestion_deposits \
                 (block_hash, height, header_time, ordinal_tx_index, ordinal_location_tag, \
                  ordinal_location_index, subject_txid, subject_vout, amount_sat, receiver, \
                  l2_contract, call_data, sender_script, encoding, source_wtxid) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
            )
            .bind(block)
            .bind(plan.anchor.height as i64)
            .bind(plan.anchor.time as i64)
            .bind(d.ordinal.tx_index as i64)
            .bind(d.ordinal.location.wire_tag() as i16)
            .bind(d.ordinal.location.index() as i64)
            .bind(d.subject.txid.as_byte_array().to_vec())
            .bind(d.subject.vout as i64)
            .bind(d.amount.to_sat() as i64)
            .bind(d.receiver.to_vec())
            .bind(d.l2_contract.to_vec())
            .bind(d.call_data.clone())
            .bind(d.sender_script.as_ref().map(|s| s.as_bytes().to_vec()))
            .bind(d.encoding.wire_tag() as i16)
            .bind(source_wtxid.as_byte_array().to_vec())
            .execute(&mut **tx)
            .await
            .map_err(infra)?;
        }
        ProtocolEvent::L1BatchDAReference(b) => {
            sqlx::query(
                "INSERT INTO via_ingestion_batch_refs \
                 (block_hash, subject_txid, l1_batch_index, l1_batch_hash, prev_l1_batch_hash, \
                  da_identifier, blob_id) VALUES ($1,$2,$3,$4,$5,$6,$7)",
            )
            .bind(block)
            .bind(b.subject_txid.as_byte_array().to_vec())
            .bind(b.l1_batch_index as i64)
            .bind(b.l1_batch_hash.to_vec())
            .bind(b.prev_l1_batch_hash.to_vec())
            .bind(&b.da_identifier)
            .bind(&b.blob_id)
            .execute(&mut **tx)
            .await
            .map_err(infra)?;
        }
        ProtocolEvent::ProofDAReference(p) => {
            sqlx::query(
                "INSERT INTO via_ingestion_proof_refs \
                 (block_hash, subject_txid, da_identifier, blob_id, batch_reveal_txid, \
                  l1_batch_index, l1_batch_hash) VALUES ($1,$2,$3,$4,$5,$6,$7)",
            )
            .bind(block)
            .bind(p.subject_txid.as_byte_array().to_vec())
            .bind(&p.da_identifier)
            .bind(&p.blob_id)
            .bind(p.batch.reveal_txid.as_byte_array().to_vec())
            .bind(p.batch.l1_batch_index as i64)
            .bind(p.batch.l1_batch_hash.to_vec())
            .execute(&mut **tx)
            .await
            .map_err(infra)?;
        }
        ProtocolEvent::ValidatorAttestation(a) => {
            sqlx::query(
                "INSERT INTO via_ingestion_votes \
                 (block_hash, subject_txid, reference_txid, attester_script, ok, l1_batch_index) \
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(block)
            .bind(a.subject_txid.as_byte_array().to_vec())
            .bind(a.reference_txid.as_byte_array().to_vec())
            .bind(a.attester_script.as_bytes().to_vec())
            .bind(a.ok)
            .bind(a.batch.l1_batch_index as i64)
            .execute(&mut **tx)
            .await
            .map_err(infra)?;
        }
        ProtocolEvent::SystemBootstrapping(b) => {
            insert_wallet_history(tx, block, b.subject_txid, &b.wallets).await?;
            insert_protocol_version(tx, block, b.protocol_version).await?;
        }
        ProtocolEvent::SystemContractUpgradeProposal(_) => {}
        ProtocolEvent::SystemContractUpgradeActivation(a) => {
            insert_protocol_version(tx, block, a.proposal.version).await?;
        }
        ProtocolEvent::BridgeWithdrawal(w) => {
            for (index, wd) in w.withdrawals.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO via_ingestion_withdrawals \
                     (block_hash, subject_txid, withdrawal_index, l2_id, l2_tx_event_index, \
                      receiver_script, amount_sat) VALUES ($1,$2,$3,$4,$5,$6,$7)",
                )
                .bind(block)
                .bind(w.subject_txid.as_byte_array().to_vec())
                .bind(index as i64)
                .bind(wd.l2_id.to_vec())
                .bind(wd.l2_tx_event_index as i32)
                .bind(wd.receiver_script.as_bytes().to_vec())
                .bind(wd.amount.to_sat() as i64)
                .execute(&mut **tx)
                .await
                .map_err(infra)?;
            }
        }
        ProtocolEvent::UpdateBridgeProposal(_) => {}
        ProtocolEvent::WalletRotation(r) => match &r.rotation {
            via_btc_ingestion::WalletRotation::Sequencer { new_script } => {
                insert_wallet_row(tx, block, r.subject_txid, 1, new_script, None).await?;
            }
            via_btc_ingestion::WalletRotation::Governance { new_script } => {
                insert_wallet_row(tx, block, r.subject_txid, 2, new_script, None).await?;
            }
            via_btc_ingestion::WalletRotation::Bridge { new_bridge_script, new_verifier_scripts, .. } => {
                insert_wallet_row(tx, block, r.subject_txid, 0, new_bridge_script, None).await?;
                for (pos, script) in new_verifier_scripts.iter().enumerate() {
                    insert_wallet_row(tx, block, r.subject_txid, 3, script, Some(pos as i64)).await?;
                }
            }
        },
    }
    Ok(())
}

async fn insert_wallet_history(
    tx: &mut PgTx<'_, Postgres>, block: &[u8], subject: Txid, wallets: &via_btc_ingestion::WalletSet,
) -> Result<(), InfraError> {
    insert_wallet_row(tx, block, subject, 0, &wallets.bridge, None).await?;
    insert_wallet_row(tx, block, subject, 1, &wallets.sequencer, None).await?;
    insert_wallet_row(tx, block, subject, 2, &wallets.governance, None).await?;
    for (pos, script) in wallets.verifiers.iter().enumerate() {
        insert_wallet_row(tx, block, subject, 3, script, Some(pos as i64)).await?;
    }
    Ok(())
}

async fn insert_wallet_row(
    tx: &mut PgTx<'_, Postgres>, block: &[u8], subject: Txid, role: i16, script: &ScriptBuf, position: Option<i64>,
) -> Result<(), InfraError> {
    sqlx::query(
        "INSERT INTO via_ingestion_wallet_history \
         (block_hash, subject_txid, role, script, verifier_position) \
         VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
    )
    .bind(block)
    .bind(subject.as_byte_array().to_vec())
    .bind(role)
    .bind(script.as_bytes().to_vec())
    .bind(position)
    .execute(&mut **tx)
    .await
    .map_err(infra)?;
    Ok(())
}

async fn insert_protocol_version(
    tx: &mut PgTx<'_, Postgres>, block: &[u8], version: via_btc_ingestion::ProtocolVersionTag,
) -> Result<(), InfraError> {
    sqlx::query(
        "INSERT INTO via_ingestion_protocol_versions (block_hash, version_minor, version_patch) \
         VALUES ($1,$2,$3) ON CONFLICT DO NOTHING",
    )
    .bind(block)
    .bind(version.minor as i64)
    .bind(version.patch as i64)
    .execute(&mut **tx)
    .await
    .map_err(infra)?;
    Ok(())
}

/// The production local-observation reader over the same pool.
pub struct PgObservationReader {
    pool: PgPool,
}

impl PgObservationReader {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn read_infra(e: impl std::fmt::Display) -> ObservationReadError {
    ObservationReadError::Infrastructure(e.to_string())
}

#[async_trait]
impl ObservationReader for PgObservationReader {
    async fn canonical_observed_tx(&self, txid: &Txid) -> Result<Option<CanonicalObservedTx>, ObservationReadError> {
        let inc: Option<(Vec<u8>, i64, i64, Vec<u8>)> = sqlx::query_as(
            "SELECT block_hash, height, tx_index, wtxid FROM via_ingestion_inclusions \
             WHERE txid = $1 AND canonical LIMIT 1",
        )
        .bind(txid.as_byte_array().to_vec())
        .fetch_optional(&self.pool)
        .await
        .map_err(read_infra)?;
        let Some((block_hash, height, tx_index, wtxid)) = inc else { return Ok(None) };
        let raw: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT raw FROM via_ingestion_raw_variants WHERE txid = $1 AND wtxid = $2")
                .bind(txid.as_byte_array().to_vec())
                .bind(&wtxid)
                .fetch_optional(&self.pool)
                .await
                .map_err(read_infra)?;
        let Some((raw,)) = raw else {
            return Err(ObservationReadError::Corrupt(format!("inclusion for {txid} has no stored raw variant")));
        };
        let variant = RawTxVariant::from_raw(raw).map_err(|e| ObservationReadError::Corrupt(e.to_string()))?;
        if variant.txid() != *txid {
            return Err(ObservationReadError::Corrupt("stored bytes decode to a different txid".into()));
        }
        let inclusion = via_btc_ingestion::Inclusion {
            block_hash: BlockHash::from_byte_array(block_hash.try_into().map_err(|_| read_infra("bad stored hash"))?),
            height: height as u64,
            tx_index: tx_index as u32,
            txid: *txid,
            wtxid: variant.wtxid(),
        };
        let stored_wtxid: [u8; 32] = wtxid.try_into().map_err(|_| read_infra("bad stored wtxid"))?;
        if variant.wtxid() != Wtxid::from_byte_array(stored_wtxid) {
            return Err(ObservationReadError::Corrupt("stored bytes decode to a different wtxid".into()));
        }
        CanonicalObservedTx::new(inclusion, variant).map(Some).map_err(ObservationReadError::Corrupt)
    }

    async fn raw_variants(&self, txid: &Txid) -> Result<Vec<RawTxVariant>, ObservationReadError> {
        let rows: Vec<(Vec<u8>,)> = sqlx::query_as("SELECT raw FROM via_ingestion_raw_variants WHERE txid = $1")
            .bind(txid.as_byte_array().to_vec())
            .fetch_all(&self.pool)
            .await
            .map_err(read_infra)?;
        rows.into_iter()
            .map(|(raw,)| RawTxVariant::from_raw(raw).map_err(|e| ObservationReadError::Corrupt(e.to_string())))
            .collect()
    }

    async fn tracked_output(&self, outpoint: &OutPoint) -> Result<Option<TrackedOutputCreate>, ObservationReadError> {
        let row: Option<(i64, Vec<u8>, i16)> = sqlx::query_as(
            "SELECT value_sat, script, role FROM via_ingestion_tracked_outputs \
             WHERE txid = $1 AND vout = $2",
        )
        .bind(outpoint.txid.as_byte_array().to_vec())
        .bind(outpoint.vout as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(read_infra)?;
        Ok(row.map(|(value, script, role)| TrackedOutputCreate {
            outpoint: *outpoint,
            value: Amount::from_sat(value as u64),
            script_pubkey: ScriptBuf::from_bytes(script),
            role: match role {
                0 => TrackedRole::Bridge,
                1 => TrackedRole::Sequencer,
                _ => TrackedRole::Governance,
            },
        }))
    }
}

/// Marks an effect as consumed downstream (sealed into a batch, finalized
/// vote). Production hooks and test harnesses both call this.
pub async fn mark_consumed(pool: &PgPool, effect: &EffectId) -> Result<(), InfraError> {
    sqlx::query(
        "INSERT INTO via_ingestion_consumed_effects (block_hash, effect_key) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(effect.block_hash.as_byte_array().to_vec())
    .bind(effect_key(effect))
    .execute(pool)
    .await
    .map_err(infra)?;
    Ok(())
}
