use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::{Core, CoreDal};

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

// jscpd:ignore-start Independent database owners retain concrete transactions, role prerequisites and schema migrations.
impl ViaVotesDal<'_, '_> {
    pub async fn record_reader_proof(
        &mut self, proof: bitcoin::Txid, batch: u32, payload: &serde_json::Value,
        inclusion: via_reorg::chain::MessageRef<bitcoin::BlockHash,bitcoin::Txid>,
    ) -> DalResult<i64> {
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("record_reader_proof").constraint_error(error))?;
        use bitcoin::hashes::Hash;
        use zksync_db_connection::instrument::Instrumented;
        if !self.storage.in_transaction() {
            return Err(Instrumented::new("record_reader_proof").constraint_error(anyhow::anyhow!("proof requires an effects transaction")));
        }
        let (set_id, _) = self.storage.via_wallet_dal().verifier_set_before(inclusion.block.height).await?;
        let proof_id: i64 = sqlx::query_scalar("INSERT INTO via_proof_evidence(proof_txid,l1_batch_number,immutable_payload)
            VALUES($1,$2,$3) ON CONFLICT(proof_txid) DO UPDATE
            SET immutable_payload=via_btc_immutable_payload(via_proof_evidence.immutable_payload,EXCLUDED.immutable_payload)
            RETURNING id")
            .bind(proof.as_byte_array().as_slice()).bind(i64::from(batch)).bind(serde_json::json!({"batch":batch,"message":payload}))
            .instrument("reader_proof_evidence").fetch_one(&mut *self.storage).await?;
        sqlx::query_scalar("INSERT INTO via_proof_inclusions(proof_id,carrier_txid,block_hash,block_height,tx_index,message_index,verifier_set_id)
            VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(proof_id,block_hash,tx_index,message_index) WHERE revoked_at IS NULL DO UPDATE
            SET verifier_set_id=CASE WHEN via_proof_inclusions.verifier_set_id=EXCLUDED.verifier_set_id
                AND via_proof_inclusions.carrier_txid=EXCLUDED.carrier_txid AND via_proof_inclusions.block_height=EXCLUDED.block_height
                THEN via_proof_inclusions.verifier_set_id ELSE NULL END RETURNING id")
            .bind(proof_id).bind(inclusion.carrier_txid.as_byte_array().as_slice())
            .bind(inclusion.block.block_hash.as_byte_array().as_slice()).bind(i64::from(inclusion.block.height))
            .bind(i64::from(inclusion.tx_index)).bind(i64::from(inclusion.message_index)).bind(set_id)
            .instrument("reader_proof_inclusion").fetch_one(self.storage).await
    }

    pub async fn record_reader_vote(
        &mut self, identity: bitcoin::Txid, proof: bitcoin::Txid, verifier: &str, approved: bool,
        payload: &serde_json::Value,
        inclusion: via_reorg::chain::MessageRef<bitcoin::BlockHash,bitcoin::Txid>,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("record_reader_vote").constraint_error(error))?;
        use bitcoin::hashes::Hash;
        use zksync_db_connection::instrument::Instrumented;
        if !self.storage.in_transaction() {
            return Err(Instrumented::new("record_reader_vote").constraint_error(anyhow::anyhow!("vote requires an effects transaction")));
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
        &mut self, proof_inclusion: i64, before: [i64;3], threshold: f64,
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
        via_reorg::votes::tally(&proof,Some(&members),&votes,threshold)
            .map_err(|error| Instrumented::new("reader_tally").constraint_error(anyhow::anyhow!("{error:?}")))
    }

    /// Rotation admission ignores local asynchronous zk-worker completion.
    pub async fn prefix_has_unfinalized_proof(&mut self, before: [i64;3], threshold: f64) -> DalResult<bool> {
        let inclusions: Vec<i64> = sqlx::query("SELECT i.id FROM via_proof_inclusions i
            JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE i.revoked_at IS NULL AND (i.block_height,i.tx_index,i.message_index)<($1,$2,$3)")
            .bind(before[0]).bind(before[1]).bind(before[2]).try_map(|row: sqlx::postgres::PgRow| { use sqlx::Row; row.try_get::<i64,_>("id") }).instrument("rotation_prefix_proofs")
            .fetch_all(&mut *self.storage).await?;
        for inclusion in inclusions {
            if self.reader_tally(inclusion,before,threshold).await?.finalized.is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn recompute_reader_finalization(&mut self, through: u32, threshold: f64) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("recompute_reader_finalization").constraint_error(error))?;
        use sqlx::Row;
        let inclusions = sqlx::query("SELECT i.id,i.verifier_set_id FROM via_proof_inclusions i
            JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE i.revoked_at IS NULL AND i.block_height<=$1")
            .bind(i64::from(through)).try_map(|row: sqlx::postgres::PgRow| Ok((row.try_get::<i64,_>("id")?,row.try_get::<i64,_>("verifier_set_id")?)))
            .instrument("reader_finalization_inputs").fetch_all(&mut *self.storage).await?;
        for (id,set_id) in inclusions {
            let decision = self.reader_tally(id,[i64::from(through)+1,0,0],threshold).await?.finalized;
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
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("revoke_reader_inclusions_after").constraint_error(error))?;
        sqlx::query("UPDATE via_vote_inclusions SET revoked_at=NOW() WHERE block_height>$1 AND revoked_at IS NULL")
            .bind(height).instrument("revoke_reader_votes").execute(&mut *self.storage).await?;
        sqlx::query("UPDATE via_proof_inclusions SET revoked_at=NOW() WHERE block_height>$1 AND revoked_at IS NULL")
            .bind(height).instrument("revoke_reader_proofs").execute(self.storage).await?;
        Ok(())
    }
}
// jscpd:ignore-end
