use std::convert::TryInto;

use sqlx::{postgres::PgRow, Row};
use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::{
    protocol_version::{ProtocolSemanticVersion, VersionPatch},
    ProtocolVersionId, H256,
};

use crate::Verifier;

#[derive(Debug)]
pub struct ViaProtocolVersionsDal<'a, 'c> {
    pub storage: &'a mut Connection<'c, Verifier>,
}

impl ViaProtocolVersionsDal<'_, '_> {
    pub async fn versions_for_proof(
        &mut self,
        height: u32,
        upgrade: Option<H256>,
    ) -> anyhow::Result<Vec<ProtocolSemanticVersion>> {
        let rows = sqlx::query("SELECT p.minor,p.patch FROM protocol_patches p JOIN protocol_versions v ON v.id=p.minor
            JOIN via_btc_headers h ON h.number=p.bitcoin_height AND h.hash=p.bitcoin_hash
            JOIN via_btc_headers m ON m.number=v.bitcoin_height AND m.hash=v.bitcoin_hash
            WHERE h.number<$1 AND m.number<$1 AND (v.executed OR v.upgrade_tx_hash=$2)
            ORDER BY p.minor DESC,p.patch DESC")
            .bind(i64::from(height)).bind(upgrade.map(|hash| hash.as_bytes().to_vec()))
            .try_map(|row: PgRow| Ok((row.try_get::<i32,_>("minor")?,row.try_get::<i32,_>("patch")?)))
            .instrument("historical_protocol_for_proof").fetch_all(self.storage).await?;
        let mut versions = Vec::new();
        for (minor, patch) in
            rows.iter().take_while(|row| Some(row.0) == rows.first().map(|row| row.0))
        {
            versions.push(ProtocolSemanticVersion {
                minor: u16::try_from(*minor)?.try_into()?,
                patch: VersionPatch((*patch).try_into()?),
            });
        }
        Ok(versions)
    }

    pub async fn pending_upgrade_before(&mut self, height: u32) -> anyhow::Result<Option<H256>> {
        Ok(sqlx::query_scalar::<_, Vec<u8>>("SELECT v.upgrade_tx_hash FROM protocol_versions v JOIN via_btc_headers h
            ON h.number=v.bitcoin_height AND h.hash=v.bitcoin_hash WHERE NOT v.executed AND h.number<$1 ORDER BY v.id LIMIT 1")
            .bind(i64::from(height)).instrument("historical_pending_upgrade").fetch_optional(self.storage).await?.map(|raw|H256::from_slice(&raw)))
    }

    pub async fn save_protocol_version(
        &mut self,
        version: ProtocolSemanticVersion,
        bootloader_code_hash: &[u8],
        default_account_code_hash: &[u8],
        upgrade_tx_hash: &[u8],
        recursion_scheduler_level_vk_hash: &[u8],
    ) -> anyhow::Result<()> {
        use via_btc_client::indexer::replay::ReplayConflict;
        let mut db_transaction = self.storage.start_transaction().await?;
        if let Some(saved) = sqlx::query!(
            "SELECT bootloader_code_hash, default_account_code_hash,
            upgrade_tx_hash, recursion_scheduler_level_vk_hash FROM protocol_versions WHERE id=$1",
            version.minor as i32
        )
        .instrument("protocol_version_replay")
        .fetch_optional(&mut db_transaction)
        .await?
        {
            for (field, equal) in [
                ("bootloader_code_hash", saved.bootloader_code_hash == bootloader_code_hash),
                (
                    "default_account_code_hash",
                    saved.default_account_code_hash == default_account_code_hash,
                ),
                ("upgrade_tx_hash", saved.upgrade_tx_hash == upgrade_tx_hash),
                (
                    "recursion_scheduler_level_vk_hash",
                    saved.recursion_scheduler_level_vk_hash == recursion_scheduler_level_vk_hash,
                ),
            ] {
                anyhow::ensure!(equal, ReplayConflict { field });
            }
        }

        sqlx::query!(
            r#"
            INSERT INTO
            protocol_versions (
                id,
                bootloader_code_hash,
                default_account_code_hash,
                upgrade_tx_hash,
                recursion_scheduler_level_vk_hash,
                executed,
                created_at
            )
            VALUES
            ($1, $2, $3, $4, $5, FALSE, NOW())
            ON CONFLICT (id) DO NOTHING;
            "#,
            version.minor as i32,
            bootloader_code_hash,
            default_account_code_hash,
            upgrade_tx_hash,
            recursion_scheduler_level_vk_hash,
        )
        .instrument("save_protocol_version#minor")
        .with_arg("minor", &version.minor)
        .with_arg("bootloader_code_hash", &bootloader_code_hash)
        .with_arg("default_account_code_hash", &default_account_code_hash)
        .with_arg("upgrade_tx_hash", &upgrade_tx_hash)
        .with_arg(
            "recursion_scheduler_level_vk_hash",
            &recursion_scheduler_level_vk_hash,
        )
        .execute(&mut db_transaction)
        .await?;

        sqlx::query!(
            r#"
            INSERT INTO
            protocol_patches (minor, patch, created_at)
            VALUES
            ($1, $2, NOW())
            ON CONFLICT DO NOTHING
            "#,
            version.minor as i32,
            version.patch.0 as i32,
        )
        .instrument("save_protocol_version#patch")
        .with_arg("version", &version)
        .execute(&mut db_transaction)
        .await?;

        db_transaction.commit().await?;

        Ok(())
    }

    async fn latest_version(&mut self, query: &str) -> DalResult<Option<ProtocolSemanticVersion>> {
        let record: Option<(i32, i32)> = sqlx::query(query)
            .try_map(|row: PgRow| Ok((row.try_get("minor")?, row.try_get("patch")?)))
            .instrument("latest_semantic_version")
            .fetch_optional(self.storage)
            .await?;
        Ok(record.map(|(minor, patch)| ProtocolSemanticVersion {
            minor: (minor as u16).try_into().unwrap(),
            patch: VersionPatch(patch as u32),
        }))
    }

    pub async fn latest_semantic_version(&mut self) -> DalResult<Option<ProtocolSemanticVersion>> {
        self.latest_version(
            r#"
            SELECT
                minor,
                patch
            FROM
                protocol_patches
            ORDER BY
                minor DESC,
                patch DESC
            LIMIT
                1
            "#,
        )
        .await
    }

    /// All registered patches of one minor version, newest first.
    /// Upstream zkSync lists a minor's patches the same way but keeps only those matching the L1-authorized key.
    /// Via keeps one verification key per minor, so every patch is returned:
    /// https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/dal/src/protocol_versions_dal.rs#L320-L347
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
        self.latest_version(
            r#"
            SELECT
                minor,
                patch
            FROM
                protocol_versions pv
            LEFT JOIN protocol_patches pp ON pv.id = pp.minor
            WHERE
                pv.executed = TRUE
            ORDER BY
                minor DESC,
                patch DESC
            LIMIT
                1
            "#,
        )
        .await
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
        let record_opt = sqlx::query!(
            r#"
            SELECT
                upgrade_tx_hash
            FROM
                protocol_versions
            WHERE
                executed = FALSE
            ORDER BY
                id ASC
            LIMIT
                1
            "#,
        )
        .instrument("get_in_progress_upgrade_tx_hash")
        .fetch_optional(self.storage)
        .await?;

        if let Some(record) = record_opt {
            return Ok(Some(H256::from_slice(&record.upgrade_tx_hash)));
        }
        Ok(None)
    }

    pub async fn mark_upgrade_as_executed(&mut self, upgrade_tx_hash: &[u8]) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE protocol_versions
            SET
                executed = TRUE
            WHERE
                upgrade_tx_hash = $1
            "#,
            upgrade_tx_hash
        )
        .instrument("mark_upgrade_as_executed")
        .fetch_optional(self.storage)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConnectionPool, VerifierDal};

    #[tokio::test]
    async fn replay_preserves_executed_upgrade_and_rejects_changed_payload() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut connection = pool.connection().await?;
        let version =
            ProtocolSemanticVersion { minor: ProtocolVersionId::Version28, patch: VersionPatch(0) };
        connection
            .via_protocol_versions_dal()
            .save_protocol_version(version, &[1; 32], &[2; 32], &[3; 32], &[4; 32])
            .await?;
        connection.via_protocol_versions_dal().mark_upgrade_as_executed(&[3; 32]).await?;
        connection
            .via_protocol_versions_dal()
            .save_protocol_version(version, &[1; 32], &[2; 32], &[3; 32], &[4; 32])
            .await?;
        assert_eq!(
            connection.via_protocol_versions_dal().latest_protocol_semantic_version().await?,
            Some(version)
        );
        let conflict = connection
            .via_protocol_versions_dal()
            .save_protocol_version(version, &[9; 32], &[2; 32], &[3; 32], &[4; 32])
            .await
            .unwrap_err();
        assert_eq!(
            conflict
                .downcast_ref::<via_btc_client::indexer::replay::ReplayConflict>()
                .unwrap()
                .field,
            "bootloader_code_hash"
        );
        assert_eq!(
            connection.via_protocol_versions_dal().latest_protocol_semantic_version().await?,
            Some(version)
        );
        Ok(())
    }
}
