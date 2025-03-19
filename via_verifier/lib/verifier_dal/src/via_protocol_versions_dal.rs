use std::convert::TryInto;

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
    pub async fn save_protocol_version(
        &mut self,
        version: ProtocolSemanticVersion,
        bootloader_code_hash: &[u8],
        default_account_code_hash: &[u8],
        upgrade_tx_hash: &[u8],
    ) -> DalResult<()> {
        let mut db_transaction = self.storage.start_transaction().await?;

        sqlx::query!(
            r#"
            INSERT INTO
                protocol_versions (
                    id,
                    bootloader_code_hash,
                    default_account_code_hash,
                    upgrade_tx_hash,
                    executed,
                    created_at
                )
            VALUES
                ($1, $2, $3, $4, FALSE, NOW())
            ON CONFLICT (id) DO
            UPDATE
            SET
                upgrade_tx_hash = EXCLUDED.upgrade_tx_hash,
                executed = FALSE;
            "#,
            version.minor as i32,
            bootloader_code_hash,
            default_account_code_hash,
            upgrade_tx_hash,
        )
        .instrument("save_protocol_version#minor")
        .with_arg("minor", &version.minor)
        .with_arg("bootloader_code_hash", &bootloader_code_hash)
        .with_arg("default_account_code_hash", &default_account_code_hash)
        .with_arg("upgrade_tx_hash", &upgrade_tx_hash)
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

    pub async fn latest_semantic_version(&mut self) -> DalResult<Option<ProtocolSemanticVersion>> {
        let record_opt = sqlx::query!(
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
            "#
        )
        .instrument("latest_semantic_version")
        .fetch_optional(self.storage)
        .await?;

        if let Some(record) = record_opt {
            return Ok(Some(ProtocolSemanticVersion {
                minor: (record.minor as u16).try_into().unwrap(),
                patch: VersionPatch(record.patch as u32),
            }));
        }
        Ok(None)
    }

    pub async fn latest_protocol_semantic_version(
        &mut self,
    ) -> DalResult<Option<ProtocolSemanticVersion>> {
        let record_opt = sqlx::query!(
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
            "#
        )
        .instrument("latest_semantic_version")
        .fetch_optional(self.storage)
        .await?;

        if let Some(record) = record_opt {
            return Ok(Some(ProtocolSemanticVersion {
                minor: (record.minor as u16).try_into().unwrap(),
                patch: VersionPatch(record.patch as u32),
            }));
        }
        Ok(None)
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
        .instrument("get_in_progress_upgrade_tx_hash")
        .fetch_optional(self.storage)
        .await?;
        Ok(())
    }
}
