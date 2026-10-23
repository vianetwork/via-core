use sqlx::{postgres::PgRow, Row};
use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::{
    protocol_version::{ProtocolSemanticVersion, VersionPatch},
    ProtocolVersionId, H256,
};

use crate::{Verifier, VerifierDal};

#[cfg(test)]
mod replay_tests {
    use super::*;
    use crate::{ConnectionPool, VerifierDal};

    async fn save_fixture(
        storage: &mut Connection<'_, Verifier>,
        version: ProtocolSemanticVersion,
        fields: &[[u8; 32]; 4],
    ) -> DalResult<()> {
        storage
            .via_protocol_versions_dal()
            .save_protocol_version(version, &fields[0], &fields[1], &fields[2], &fields[3])
            .await
    }

    #[tokio::test]
    async fn bootstrap_origin_requires_full_code_upgrade_and_verifier_equivalence() {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut connection = pool.connection().await.unwrap();
        let mut tx = connection.start_transaction().await.unwrap();
        let version = ProtocolSemanticVersion::new(ProtocolVersionId::latest(), VersionPatch(0));
        let original = [[1; 32], [2; 32], [3; 32], [4; 32]];
        tx.via_protocol_versions_dal()
            .save_protocol_version(version, &original[0], &original[1], &original[2], &original[3])
            .await
            .unwrap();
        for field in 0..4 {
            let mut changed = original;
            changed[field] = [9; 32];
            assert!(tx
                .via_protocol_versions_dal()
                .mark_via_bootstrap_version(
                    version,
                    &changed[0],
                    &changed[1],
                    &changed[2],
                    &changed[3]
                )
                .await
                .is_err());
            let classified: bool = sqlx::query_scalar("SELECT via_bootstrap FROM protocol_patches")
                .fetch_one(tx.conn())
                .await
                .unwrap();
            assert!(!classified);
        }
        tx.via_protocol_versions_dal()
            .mark_via_bootstrap_version(
                version,
                &original[0],
                &original[1],
                &original[2],
                &original[3],
            )
            .await
            .unwrap();
        let classified: bool = sqlx::query_scalar("SELECT via_bootstrap FROM protocol_patches")
            .fetch_one(tx.conn())
            .await
            .unwrap();
        assert!(classified);
    }

    #[tokio::test]
    async fn executed_upgrade_replay_preserves_completion_and_rejects_changed_payload() {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut connection = pool.connection().await.unwrap();
        let mut storage = connection.start_transaction().await.unwrap();
        let version = ProtocolSemanticVersion::new(ProtocolVersionId::latest(), VersionPatch(0));
        let original = [[1; 32], [2; 32], [3; 32], [4; 32]];
        save_fixture(&mut storage, version, &original).await.unwrap();
        storage.via_protocol_versions_dal().mark_upgrade_as_executed(&original[2]).await.unwrap();
        save_fixture(&mut storage, version, &original).await.unwrap();
        assert_eq!(
            storage.via_protocol_versions_dal().get_in_progress_upgrade_tx_hash().await.unwrap(),
            None
        );
        let executed: bool =
            sqlx::query_scalar("SELECT executed FROM protocol_versions WHERE id=$1")
                .bind(version.minor as i32)
                .fetch_one(storage.conn())
                .await
                .unwrap();
        assert!(executed);
        for field in 0..original.len() {
            let mut changed = original;
            changed[field] = [9; 32];
            assert!(storage
                .via_protocol_versions_dal()
                .save_protocol_version(version, &changed[0], &changed[1], &changed[2], &changed[3],)
                .await
                .is_err());
        }
        let patch = ProtocolSemanticVersion { patch: VersionPatch(1), ..version };
        storage
            .via_protocol_versions_dal()
            .save_protocol_version(patch, &original[0], &original[1], &[5; 32], &original[3])
            .await
            .unwrap();
        storage.via_protocol_versions_dal().mark_upgrade_as_executed(&[5; 32]).await.unwrap();
        save_fixture(&mut storage, version, &original).await.unwrap();
        let hash: Vec<u8> =
            sqlx::query_scalar("SELECT upgrade_tx_hash FROM protocol_versions WHERE id=$1")
                .bind(version.minor as i32)
                .fetch_one(storage.conn())
                .await
                .unwrap();
        assert_eq!(hash, vec![5; 32]);
        assert_eq!(
            storage.via_protocol_versions_dal().get_in_progress_upgrade_tx_hash().await.unwrap(),
            None
        );
    }
}

#[derive(Debug)]
pub struct ViaProtocolVersionsDal<'a, 'c> {
    pub storage: &'a mut Connection<'c, Verifier>,
}

impl ViaProtocolVersionsDal<'_, '_> {
    pub async fn save_protocol_version(
        &mut self,
        version: ProtocolSemanticVersion,
        bootloader_code_hash: &[u8],
        default_account_code_hash: &[u8],
        upgrade_tx_hash: &[u8],
        recursion_scheduler_level_vk_hash: &[u8],
    ) -> DalResult<()> {
        let mut db_transaction = self.storage.start_transaction().await?;
        db_transaction.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("save_protocol_version")
                .constraint_error(error)
        })?;

        // Equal replay is checked by full semantic version; it never resets executed state.
        // Adapted guard: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/decentralized_upgrades.rs#L107-L113
        let payload = serde_json::json!({
            "bootloader_code_hash":bootloader_code_hash,
            "default_account_code_hash":default_account_code_hash,
            "upgrade_tx_hash":upgrade_tx_hash,
            "recursion_scheduler_level_vk_hash":recursion_scheduler_level_vk_hash,
        });
        sqlx::query("INSERT INTO protocol_versions(id,bootloader_code_hash,default_account_code_hash,upgrade_tx_hash,recursion_scheduler_level_vk_hash,executed,created_at)
            VALUES($1,$2,$3,$4,$5,FALSE,NOW()) ON CONFLICT(id) DO NOTHING")
            .bind(version.minor as i32).bind(bootloader_code_hash).bind(default_account_code_hash)
            .bind(upgrade_tx_hash).bind(recursion_scheduler_level_vk_hash)
            .instrument("save_protocol_version#minor").execute(&mut db_transaction).await?;
        sqlx::query("SELECT id FROM protocol_versions WHERE id=$1 FOR UPDATE")
            .bind(version.minor as i32)
            .instrument("lock_protocol_version")
            .execute(&mut db_transaction)
            .await?;
        let inserted: Option<i32> = sqlx::query_scalar(
            "INSERT INTO protocol_patches(minor,patch,immutable_payload,upgrade_tx_hash,created_at)
            VALUES($1,$2,$3,$4,NOW()) ON CONFLICT(minor,patch) DO NOTHING RETURNING 1",
        )
        .bind(version.minor as i32)
        .bind(version.patch.0 as i32)
        .bind(&payload)
        .bind(upgrade_tx_hash)
        .instrument("save_protocol_version#patch")
        .fetch_optional(&mut db_transaction)
        .await?;
        if inserted.is_some() {
            // Preserve the existing transition for a new semantic version, including the executed flag.
            sqlx::query(
                "UPDATE protocol_versions SET upgrade_tx_hash=$2,executed=FALSE WHERE id=$1",
            )
            .bind(version.minor as i32)
            .bind(upgrade_tx_hash)
            .instrument("new_protocol_patch")
            .execute(&mut db_transaction)
            .await?;
        } else {
            sqlx::query("SELECT via_btc_immutable_payload(immutable_payload,$3) FROM protocol_patches WHERE minor=$1 AND patch=$2")
                .bind(version.minor as i32).bind(version.patch.0 as i32).bind(&payload)
                .instrument("replay_protocol_version").execute(&mut db_transaction).await?;
        }

        db_transaction.commit().await?;

        Ok(())
    }

    async fn latest_version(
        &mut self,
        query: &str,
        before: Option<i64>,
    ) -> DalResult<Option<ProtocolSemanticVersion>> {
        let query = sqlx::query(query);
        let query = if let Some(height) = before { query.bind(height) } else { query };
        query
            .try_map(|row: PgRow| {
                let minor: u16 = u16::try_from(row.try_get::<i32, _>("minor")?)
                    .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
                let patch = u32::try_from(row.try_get::<i32, _>("patch")?)
                    .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
                Ok(ProtocolSemanticVersion {
                    minor: ProtocolVersionId::try_from(minor)
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
                    patch: VersionPatch(patch),
                })
            })
            .instrument("latest_semantic_version")
            .fetch_optional(self.storage)
            .await
    }

    pub async fn latest_semantic_version(&mut self) -> DalResult<Option<ProtocolSemanticVersion>> {
        self.latest_protocol_before(i64::from(u32::MAX) + 1, false).await
    }

    pub async fn latest_semantic_version_before(
        &mut self,
        height: u32,
    ) -> DalResult<Option<ProtocolSemanticVersion>> {
        self.latest_protocol_before(i64::from(height), false).await
    }

    pub async fn latest_executed_version_before(
        &mut self,
        height: u32,
    ) -> DalResult<Option<ProtocolSemanticVersion>> {
        self.latest_protocol_before(i64::from(height), true).await
    }

    async fn latest_protocol_before(
        &mut self,
        height: i64,
        executed: bool,
    ) -> DalResult<Option<ProtocolSemanticVersion>> {
        let executed = if executed { "AND pp.executed" } else { "" };
        let query=format!("SELECT pp.minor,pp.patch FROM protocol_patches pp WHERE (pp.via_bootstrap OR EXISTS(
            SELECT 1 FROM via_upgrade_evidence e JOIN via_upgrade_inclusions i ON i.upgrade_id=e.id
            JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE e.minor=pp.minor AND e.patch=pp.patch AND i.revoked_at IS NULL AND i.block_height<$1))
            {executed} ORDER BY pp.minor DESC,pp.patch DESC LIMIT 1");
        self.latest_version(&query, Some(height)).await
    }

    pub async fn semantic_versions_of_minor(
        &mut self,
        minor: ProtocolVersionId,
    ) -> DalResult<Vec<ProtocolSemanticVersion>> {
        let patches =
            sqlx::query("SELECT patch FROM protocol_patches WHERE minor = $1 ORDER BY patch DESC")
                .bind(minor as i32)
                .map(|row: PgRow| row.get::<i32, _>("patch"))
                .instrument("semantic_versions_of_minor")
                .fetch_all(self.storage)
                .await?;
        Ok(patches
            .into_iter()
            .map(|patch| ProtocolSemanticVersion { minor, patch: VersionPatch(patch as u32) })
            .collect())
    }

    pub async fn latest_protocol_semantic_version(
        &mut self,
    ) -> DalResult<Option<ProtocolSemanticVersion>> {
        self.latest_protocol_before(i64::from(u32::MAX) + 1, true).await
    }

    pub async fn get_recursion_scheduler_level_vk_hash(
        &mut self,
        protocol_version_id: ProtocolVersionId,
    ) -> DalResult<H256> {
        let record = sqlx::query!(
            r#"
            SELECT
                recursion_scheduler_level_vk_hash
            FROM
                protocol_versions
            WHERE
                id = $1
            LIMIT
                1
            "#,
            protocol_version_id as i32
        )
        .instrument("get_recursion_scheduler_level_vk_hash")
        .fetch_one(self.storage)
        .await?;

        Ok(H256::from_slice(&record.recursion_scheduler_level_vk_hash))
    }

    pub async fn get_protocol_base_system_contracts(
        &mut self,
        protocol_version_id: ProtocolVersionId,
    ) -> DalResult<Option<(H256, H256)>> {
        let record_opt = sqlx::query!(
            r#"
            SELECT
                bootloader_code_hash,
                default_account_code_hash
            FROM
                protocol_versions pv
            LEFT JOIN protocol_patches pp ON pv.id = pp.minor
            WHERE
                pv.id = $1
            ORDER BY
                pp.minor DESC,
                pp.patch DESC
            LIMIT
                1
            "#,
            protocol_version_id as i32
        )
        .instrument("get_protocol_upgrade_tx")
        .with_arg("protocol_version_id", &protocol_version_id)
        .fetch_optional(self.storage)
        .await?;

        if let Some(record) = record_opt {
            return Ok(Some((
                H256::from_slice(&record.bootloader_code_hash),
                H256::from_slice(&record.default_account_code_hash),
            )));
        }
        Ok(None)
    }

    pub async fn get_protocol_upgrade_tx(
        &mut self,
        protocol_version_id: ProtocolVersionId,
    ) -> DalResult<Option<H256>> {
        let record_opt = sqlx::query!(
            r#"
            SELECT
                upgrade_tx_hash
            FROM
                protocol_versions
            WHERE
                id = $1
            "#,
            protocol_version_id as i32
        )
        .instrument("get_protocol_upgrade_tx")
        .with_arg("protocol_version_id", &protocol_version_id)
        .fetch_optional(self.storage)
        .await?;

        if let Some(record) = record_opt {
            return Ok(Some(H256::from_slice(&record.upgrade_tx_hash)));
        }
        Ok(None)
    }

    pub async fn get_in_progress_upgrade_tx_hash(&mut self) -> DalResult<Option<H256>> {
        self.pending_upgrade_before(i64::from(u32::MAX) + 1).await
    }

    pub async fn get_in_progress_upgrade_tx_hash_before(
        &mut self,
        height: u32,
    ) -> DalResult<Option<H256>> {
        self.pending_upgrade_before(i64::from(height)).await
    }

    async fn pending_upgrade_before(&mut self, height: i64) -> DalResult<Option<H256>> {
        let hash:Option<Vec<u8>>=sqlx::query_scalar("SELECT pp.upgrade_tx_hash FROM protocol_patches pp
            WHERE NOT pp.executed AND pp.upgrade_tx_hash IS NOT NULL AND (pp.via_bootstrap OR EXISTS(
                SELECT 1 FROM via_upgrade_evidence e JOIN via_upgrade_inclusions i ON i.upgrade_id=e.id
                JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
                WHERE e.minor=pp.minor AND e.patch=pp.patch AND i.revoked_at IS NULL AND i.block_height<$1))
            ORDER BY pp.minor DESC,pp.patch DESC LIMIT 1")
            .bind(height).instrument("pending_historical_upgrade").fetch_optional(self.storage).await?;
        Ok(hash.map(|hash| H256::from_slice(&hash)))
    }

    pub async fn semantic_versions_of_minor_before(
        &mut self,
        minor: ProtocolVersionId,
        height: u32,
    ) -> DalResult<Vec<ProtocolSemanticVersion>> {
        let patches=sqlx::query("SELECT pp.patch FROM protocol_patches pp WHERE pp.minor=$1 AND (pp.via_bootstrap OR EXISTS(
            SELECT 1 FROM via_upgrade_evidence e JOIN via_upgrade_inclusions i ON i.upgrade_id=e.id
            JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE e.minor=pp.minor AND e.patch=pp.patch AND i.revoked_at IS NULL AND i.block_height<$2))
            ORDER BY pp.patch DESC")
            .bind(minor as i32).bind(i64::from(height)).try_map(|row:PgRow| row.try_get::<i32,_>("patch"))
            .instrument("historical_protocol_patches").fetch_all(self.storage).await?;
        patches
            .into_iter()
            .map(|patch| {
                Ok(ProtocolSemanticVersion {
                    minor,
                    patch: VersionPatch(u32::try_from(patch).map_err(|error| {
                        zksync_db_connection::instrument::Instrumented::new(
                            "historical_protocol_patches",
                        )
                        .arg_error("patch", error)
                    })?),
                })
            })
            .collect()
    }

    pub async fn mark_upgrade_as_executed(&mut self, hash: &[u8]) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("mark_upgrade_as_executed")
                .constraint_error(error)
        })?;
        sqlx::query("UPDATE protocol_patches SET executed=TRUE WHERE upgrade_tx_hash=$1")
            .bind(hash)
            .instrument("execute_semantic_protocol_version")
            .execute(&mut *self.storage)
            .await?;
        sqlx::query("UPDATE protocol_versions SET executed=TRUE WHERE upgrade_tx_hash=$1")
            .bind(hash)
            .instrument("project_current_protocol_execution")
            .execute(self.storage)
            .await?;
        Ok(())
    }
}

// jscpd:ignore-start Independent schema adapters preserve concrete protocol-version transaction ownership.
impl ViaProtocolVersionsDal<'_, '_> {
    pub async fn revoke_via_upgrades_after(&mut self, height: i64) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("revoke_upgrade_authority")
                .constraint_error(error)
        })?;
        sqlx::query("UPDATE via_upgrade_inclusions SET revoked_at=NOW() WHERE block_height>$1 AND revoked_at IS NULL")
            .bind(height).instrument("revoke_upgrade_authority").execute(self.storage).await?;
        Ok(())
    }

    pub async fn mark_via_bootstrap_version(
        &mut self,
        version: ProtocolSemanticVersion,
        bootloader: &[u8],
        default_account: &[u8],
        upgrade: &[u8],
        verifier_key: &[u8],
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("bootstrap_protocol_origin")
                .constraint_error(error)
        })?;
        let result=sqlx::query("UPDATE protocol_patches pp SET via_bootstrap=TRUE FROM protocol_versions pv
            WHERE pv.id=pp.minor AND pp.minor=$1 AND pp.patch=$2 AND pp.immutable_payload IS NOT NULL
            AND pv.bootloader_code_hash=$3 AND pv.default_account_code_hash=$4
            AND pp.upgrade_tx_hash=$5 AND pv.recursion_scheduler_level_vk_hash=$6")
            .bind(version.minor as i32).bind(version.patch.0 as i32).bind(bootloader).bind(default_account)
            .bind(upgrade).bind(verifier_key).instrument("bootstrap_protocol_origin").execute(self.storage).await?;
        if result.rows_affected() != 1 {
            return Err(zksync_db_connection::instrument::Instrumented::new(
                "bootstrap_protocol_origin",
            )
            .constraint_error(anyhow::anyhow!(
                "bootstrap protocol content does not match its proved source"
            )));
        }
        Ok(())
    }

    pub async fn record_via_upgrade_origin(
        &mut self,
        version: ProtocolSemanticVersion,
        identity: bitcoin::Txid,
        payload: &serde_json::Value,
        inclusion: via_reorg::chain::MessageRef<bitcoin::BlockHash, bitcoin::Txid>,
    ) -> DalResult<()> {
        use bitcoin::hashes::Hash;
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("record_upgrade_origin")
                .constraint_error(error)
        })?;
        let immutable = serde_json::json!({"version":version.to_string(),"message":payload});
        let evidence:i64=sqlx::query_scalar("INSERT INTO via_upgrade_evidence(txid,message_index,minor,patch,immutable_payload)
            VALUES($1,$2,$3,$4,$5) ON CONFLICT(txid,message_index) DO UPDATE
            SET immutable_payload=via_btc_immutable_payload(via_upgrade_evidence.immutable_payload,EXCLUDED.immutable_payload) RETURNING id")
            .bind(identity.as_byte_array().as_slice()).bind(i64::from(inclusion.message_index))
            .bind(version.minor as i32).bind(i64::from(version.patch.0)).bind(immutable)
            .instrument("upgrade_evidence").fetch_one(&mut *self.storage).await?;
        sqlx::query("INSERT INTO via_upgrade_inclusions(upgrade_id,carrier_txid,block_hash,block_height,tx_index,message_index)
            VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(upgrade_id,block_hash,tx_index,message_index) WHERE revoked_at IS NULL DO UPDATE
            SET carrier_txid=CASE WHEN via_upgrade_inclusions.carrier_txid=EXCLUDED.carrier_txid
                AND via_upgrade_inclusions.block_height=EXCLUDED.block_height THEN via_upgrade_inclusions.carrier_txid ELSE NULL END")
            .bind(evidence).bind(inclusion.carrier_txid.as_byte_array().as_slice()).bind(inclusion.block.block_hash.as_byte_array().as_slice())
            .bind(i64::from(inclusion.block.height)).bind(i64::from(inclusion.tx_index)).bind(i64::from(inclusion.message_index))
            .instrument("upgrade_inclusion").execute(self.storage).await?;
        Ok(())
    }
}
// jscpd:ignore-end
