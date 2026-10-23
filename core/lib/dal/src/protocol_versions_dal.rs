use std::convert::TryInto;

use sqlx::Row;

use anyhow::Context as _;
use zksync_contracts::{BaseSystemContracts, BaseSystemContractsHashes};
use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::{
    protocol_upgrade::{ProtocolUpgradeTx, ProtocolVersion},
    protocol_version::{L1VerifierConfig, ProtocolSemanticVersion, VersionPatch},
    ProtocolVersionId, H256,
};

use crate::{
    models::{
        parse_protocol_version,
        storage_protocol_version::{protocol_version_from_storage, StorageProtocolVersion},
    },
    Core, CoreDal,
};

#[derive(Debug)]
pub struct ProtocolVersionsDal<'a, 'c> {
    pub storage: &'a mut Connection<'c, Core>,
}

impl ProtocolVersionsDal<'_, '_> {
    pub async fn save_protocol_version(
        &mut self,
        version: ProtocolSemanticVersion,
        timestamp: u64,
        l1_verifier_config: L1VerifierConfig,
        base_system_contracts_hashes: BaseSystemContractsHashes,
        tx_hash: Option<H256>,
    ) -> DalResult<()> {
        let mut db_transaction = self.storage.start_transaction().await?;

        sqlx::query!(
            r#"
            INSERT INTO
            protocol_versions (
                id,
                timestamp,
                bootloader_code_hash,
                default_account_code_hash,
                evm_emulator_code_hash,
                upgrade_tx_hash,
                created_at
            )
            VALUES
            ($1, $2, $3, $4, $5, $6, NOW())
            ON CONFLICT DO NOTHING
            "#,
            version.minor as i32,
            timestamp as i64,
            base_system_contracts_hashes.bootloader.as_bytes(),
            base_system_contracts_hashes.default_aa.as_bytes(),
            base_system_contracts_hashes
                .evm_emulator
                .as_ref()
                .map(H256::as_bytes),
            tx_hash.as_ref().map(H256::as_bytes),
        )
        .instrument("save_protocol_version#minor")
        .with_arg("minor", &version.minor)
        .with_arg(
            "base_system_contracts_hashes",
            &base_system_contracts_hashes,
        )
        .with_arg("tx_hash", &tx_hash)
        .execute(&mut db_transaction)
        .await?;

        let payload = serde_json::json!({
            "timestamp":timestamp,
            "bootloader_code_hash":base_system_contracts_hashes.bootloader,
            "default_account_code_hash":base_system_contracts_hashes.default_aa,
            "evm_emulator_code_hash":base_system_contracts_hashes.evm_emulator,
            "upgrade_tx_hash":tx_hash,
            "snark_wrapper_vk_hash":l1_verifier_config.snark_wrapper_vk_hash,
        });
        sqlx::query("SELECT id FROM protocol_versions WHERE id=$1 FOR UPDATE")
            .bind(version.minor as i32).instrument("lock_protocol_version").execute(&mut db_transaction).await?;
        sqlx::query("INSERT INTO protocol_patches(minor,patch,snark_wrapper_vk_hash,immutable_payload,created_at)
            VALUES($1,$2,$3,$4,NOW()) ON CONFLICT(minor,patch) DO UPDATE
            SET immutable_payload=via_btc_immutable_payload(protocol_patches.immutable_payload,EXCLUDED.immutable_payload)")
            .bind(version.minor as i32).bind(version.patch.0 as i32)
            .bind(l1_verifier_config.snark_wrapper_vk_hash.as_bytes()).bind(payload)
            .instrument("save_protocol_version#patch").execute(&mut db_transaction).await?;

        db_transaction.commit().await?;

        Ok(())
    }

    pub async fn save_protocol_version_with_tx(
        &mut self,
        version: &ProtocolVersion,
    ) -> DalResult<()> {
        let tx_hash = version.tx.as_ref().map(|tx| tx.common_data.hash());
        let mut db_transaction = self.storage.start_transaction().await?;
        if let Some(tx) = &version.tx {
            db_transaction
                .transactions_dal()
                .insert_system_transaction(tx)
                .await?;
        }

        db_transaction
            .protocol_versions_dal()
            .save_protocol_version(
                version.version,
                version.timestamp,
                version.l1_verifier_config,
                version.base_system_contracts_hashes,
                tx_hash,
            )
            .await?;
        db_transaction.commit().await
    }

    async fn save_genesis_upgrade_tx_hash(
        &mut self,
        id: ProtocolVersionId,
        tx_hash: Option<H256>,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE protocol_versions
            SET
                upgrade_tx_hash = $1
            WHERE
                id = $2
            "#,
            tx_hash.as_ref().map(H256::as_bytes),
            id as i32,
        )
        .instrument("save_genesis_upgrade_tx_hash")
        .with_arg("id", &id)
        .with_arg("tx_hash", &tx_hash)
        .execute(self.storage)
        .await?;
        Ok(())
    }

    /// Attaches a transaction used to set ChainId to the genesis protocol version.
    /// Also inserts that transaction into the database.
    pub async fn save_genesis_upgrade_with_tx(
        &mut self,
        id: ProtocolVersionId,
        tx: &ProtocolUpgradeTx,
    ) -> DalResult<()> {
        let tx_hash = Some(tx.common_data.hash());
        let mut db_transaction = self.storage.start_transaction().await?;
        db_transaction
            .transactions_dal()
            .insert_system_transaction(tx)
            .await?;
        db_transaction
            .protocol_versions_dal()
            .save_genesis_upgrade_tx_hash(id, tx_hash)
            .await?;
        db_transaction.commit().await
    }

    pub async fn protocol_version_id_by_timestamp(
        &mut self,
        current_timestamp: u64,
    ) -> sqlx::Result<ProtocolVersionId> {
        let row = sqlx::query!(
            r#"
            SELECT
                id
            FROM
                protocol_versions
            WHERE
                timestamp <= $1
            ORDER BY
                id DESC
            LIMIT
                1
            "#,
            current_timestamp as i64
        )
        .fetch_one(self.storage.conn())
        .await?;

        ProtocolVersionId::try_from(row.id as u16).map_err(|err| sqlx::Error::Decode(err.into()))
    }

    /// Returns base system contracts' hashes. Prefer `load_base_system_contracts_by_version_id` if
    /// you also want to load the contracts themselves AND expect the contracts to be in the DB
    /// already.
    pub async fn get_base_system_contract_hashes_by_version_id(
        &mut self,
        version_id: u16,
    ) -> anyhow::Result<Option<BaseSystemContractsHashes>> {
        let row = sqlx::query!(
            r#"
            SELECT
                bootloader_code_hash,
                default_account_code_hash,
                evm_emulator_code_hash
            FROM
                protocol_versions
            WHERE
                id = $1
            "#,
            i32::from(version_id)
        )
        .instrument("get_base_system_contract_hashes_by_version_id")
        .with_arg("version_id", &version_id)
        .fetch_optional(self.storage)
        .await
        .context("cannot fetch system contract hashes")?;

        Ok(if let Some(row) = row {
            Some(BaseSystemContractsHashes {
                bootloader: H256::from_slice(&row.bootloader_code_hash),
                default_aa: H256::from_slice(&row.default_account_code_hash),
                evm_emulator: row.evm_emulator_code_hash.as_deref().map(H256::from_slice),
            })
        } else {
            None
        })
    }

    pub async fn load_base_system_contracts_by_version_id(
        &mut self,
        version_id: u16,
    ) -> anyhow::Result<Option<BaseSystemContracts>> {
        let row = sqlx::query!(
            r#"
            SELECT
                bootloader_code_hash,
                default_account_code_hash,
                evm_emulator_code_hash
            FROM
                protocol_versions
            WHERE
                id = $1
            "#,
            i32::from(version_id)
        )
        .instrument("load_base_system_contracts_by_version_id")
        .with_arg("version_id", &version_id)
        .fetch_optional(self.storage)
        .await
        .context("cannot fetch system contract hashes")?;

        Ok(if let Some(row) = row {
            let contracts = self
                .storage
                .factory_deps_dal()
                .get_base_system_contracts(
                    H256::from_slice(&row.bootloader_code_hash),
                    H256::from_slice(&row.default_account_code_hash),
                    row.evm_emulator_code_hash.as_deref().map(H256::from_slice),
                )
                .await?;
            Some(contracts)
        } else {
            None
        })
    }

    pub async fn get_protocol_version_with_latest_patch(
        &mut self,
        version_id: ProtocolVersionId,
    ) -> DalResult<Option<ProtocolVersion>> {
        let maybe_row = sqlx::query_as!(
            StorageProtocolVersion,
            r#"
            SELECT
                protocol_versions.id AS "minor!",
                protocol_versions.timestamp,
                protocol_versions.bootloader_code_hash,
                protocol_versions.default_account_code_hash,
                protocol_versions.evm_emulator_code_hash,
                protocol_patches.patch,
                protocol_patches.snark_wrapper_vk_hash
            FROM
                protocol_versions
            JOIN protocol_patches ON protocol_patches.minor = protocol_versions.id
            WHERE
                id = $1
            ORDER BY
                protocol_patches.patch DESC
            LIMIT
                1
            "#,
            version_id as i32
        )
        .instrument("get_protocol_version_with_latest_patch")
        .with_arg("version_id", &version_id)
        .fetch_optional(self.storage)
        .await?;

        let Some(row) = maybe_row else {
            return Ok(None);
        };
        let tx = self.get_protocol_upgrade_tx(version_id).await?;

        Ok(Some(protocol_version_from_storage(row, tx)))
    }

    pub async fn l1_verifier_config_for_version(
        &mut self,
        version: ProtocolSemanticVersion,
    ) -> Option<L1VerifierConfig> {
        let row = sqlx::query!(
            r#"
            SELECT
                snark_wrapper_vk_hash
            FROM
                protocol_patches
            WHERE
                minor = $1
                AND patch = $2
            "#,
            version.minor as i32,
            version.patch.0 as i32
        )
        .fetch_optional(self.storage.conn())
        .await
        .unwrap()?;
        Some(L1VerifierConfig {
            snark_wrapper_vk_hash: H256::from_slice(&row.snark_wrapper_vk_hash),
        })
    }

    pub async fn get_patch_versions_for_vk(
        &mut self,
        minor_version: ProtocolVersionId,
        snark_wrapper_vk_hash: H256,
    ) -> DalResult<Vec<VersionPatch>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                patch
            FROM
                protocol_patches
            WHERE
                minor = $1
                AND snark_wrapper_vk_hash = $2
            ORDER BY
                patch DESC
            "#,
            minor_version as i32,
            snark_wrapper_vk_hash.as_bytes()
        )
        .instrument("get_patch_versions_for_vk")
        .fetch_all(self.storage)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| VersionPatch(row.patch as u32))
            .collect())
    }

    /// Returns first patch number for the minor version.
    /// Note, that some patch numbers can be skipped, so the result is not always 0.
    pub async fn first_patch_for_version(
        &mut self,
        version_id: ProtocolVersionId,
    ) -> DalResult<Option<VersionPatch>> {
        let row = sqlx::query!(
            r#"
            SELECT
                patch
            FROM
                protocol_patches
            WHERE
                minor = $1
            ORDER BY
                patch
            LIMIT
                1
            "#,
            version_id as i32,
        )
        .instrument("first_patch_for_version")
        .fetch_optional(self.storage)
        .await?;
        Ok(row.map(|row| VersionPatch(row.patch as u32)))
    }

    pub async fn latest_semantic_version(&mut self) -> DalResult<Option<ProtocolSemanticVersion>> {
        sqlx::query!(
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
        .try_map(|row| {
            parse_protocol_version(row.minor).map(|minor| ProtocolSemanticVersion {
                minor,
                patch: (row.patch as u32).into(),
            })
        })
        .instrument("latest_semantic_version")
        .fetch_optional(self.storage)
        .await
    }

    pub async fn last_used_version_id(&mut self) -> Option<ProtocolVersionId> {
        let id = sqlx::query!(
            r#"
            SELECT
                protocol_version
            FROM
                l1_batches
            WHERE
                is_sealed
            ORDER BY
                number DESC
            LIMIT
                1
            "#
        )
        .fetch_optional(self.storage.conn())
        .await
        .unwrap()?
        .protocol_version?;

        Some((id as u16).try_into().unwrap())
    }

    pub async fn all_versions(&mut self) -> Vec<ProtocolSemanticVersion> {
        let rows = sqlx::query!(
            r#"
            SELECT
                minor,
                patch
            FROM
                protocol_patches
            "#
        )
        .fetch_all(self.storage.conn())
        .await
        .unwrap();
        rows.into_iter()
            .map(|row| ProtocolSemanticVersion {
                minor: (row.minor as u16).try_into().unwrap(),
                patch: (row.patch as u32).into(),
            })
            .collect()
    }

    pub async fn get_protocol_upgrade_tx(
        &mut self,
        protocol_version_id: ProtocolVersionId,
    ) -> DalResult<Option<ProtocolUpgradeTx>> {
        let instrumentation = Instrumented::new("get_protocol_upgrade_tx")
            .with_arg("protocol_version_id", &protocol_version_id);
        let query = sqlx::query!(
            r#"
            SELECT
                upgrade_tx_hash
            FROM
                protocol_versions
            WHERE
                id = $1
            "#,
            protocol_version_id as i32
        );

        let maybe_row = instrumentation
            .with(query)
            .fetch_optional(self.storage)
            .await?;
        let Some(upgrade_tx_hash) = maybe_row.and_then(|row| row.upgrade_tx_hash) else {
            return Ok(None);
        };
        let upgrade_tx_hash = H256::from_slice(&upgrade_tx_hash);

        let instrumentation = Instrumented::new("get_protocol_upgrade_tx#get_tx")
            .with_arg("protocol_version_id", &protocol_version_id)
            .with_arg("upgrade_tx_hash", &upgrade_tx_hash);
        let tx = self
            .storage
            .transactions_dal()
            .get_tx_by_hash(upgrade_tx_hash)
            .await?
            .ok_or_else(|| {
                instrumentation.arg_error(
                    "upgrade_tx_hash",
                    anyhow::anyhow!("upgrade transaction is not present in storage"),
                )
            })?;
        let tx = tx
            .try_into()
            .map_err(|err| instrumentation.arg_error("tx", anyhow::Error::msg(err)))?;
        Ok(Some(tx))
    }
}

// jscpd:ignore-start Independent schema adapters preserve concrete protocol-version transaction ownership.
impl ProtocolVersionsDal<'_, '_> {

    /// Runtime selection excludes retained approvals whose Bitcoin inclusion is no longer canonical.
    pub async fn latest_via_semantic_version(&mut self) -> DalResult<Option<ProtocolSemanticVersion>> {
        self.latest_via_semantic_version_before(u64::from(u32::MAX)+1).await
    }

    pub async fn latest_via_semantic_version_before(&mut self,height:u64) -> DalResult<Option<ProtocolSemanticVersion>> {
        sqlx::query("SELECT pp.minor,pp.patch FROM protocol_patches pp
            WHERE pp.via_bootstrap OR EXISTS(SELECT 1 FROM via_upgrade_evidence e
                JOIN via_upgrade_inclusions i ON i.upgrade_id=e.id
                JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
                WHERE e.minor=pp.minor AND e.patch=pp.patch AND i.revoked_at IS NULL AND i.block_height<$1)
            ORDER BY pp.minor DESC,pp.patch DESC LIMIT 1")
            .bind(i64::try_from(height).map_err(|error| Instrumented::new("latest_via_semantic_version").constraint_error(error.into()))?)
            .try_map(|row:sqlx::postgres::PgRow| Ok(ProtocolSemanticVersion {
                minor:parse_protocol_version(row.try_get("minor")?)?,
                patch:VersionPatch(u32::try_from(row.try_get::<i32,_>("patch")?).map_err(|error| sqlx::Error::Decode(Box::new(error)))?),
            })).instrument("latest_via_semantic_version").fetch_optional(self.storage).await
    }

    pub async fn via_protocol_version_id_by_timestamp(&mut self,timestamp:u64) -> sqlx::Result<ProtocolVersionId> {
        let minor:i32=sqlx::query_scalar("SELECT pv.id FROM protocol_versions pv WHERE pv.timestamp<=$1
            AND EXISTS(SELECT 1 FROM protocol_patches pp WHERE pp.minor=pv.id AND (pp.via_bootstrap OR EXISTS(
                SELECT 1 FROM via_upgrade_evidence e JOIN via_upgrade_inclusions i ON i.upgrade_id=e.id
                JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
                WHERE e.minor=pp.minor AND e.patch=pp.patch AND i.revoked_at IS NULL)))
            ORDER BY pv.id DESC LIMIT 1")
            .bind(i64::try_from(timestamp).map_err(|error| sqlx::Error::Decode(Box::new(error)))?)
            .fetch_one(self.storage.conn()).await?;
        parse_protocol_version(minor)
    }

    pub async fn revoke_via_upgrades_after(&mut self,height:i64) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("revoke_upgrade_authority").constraint_error(error))?;
        sqlx::query("UPDATE via_upgrade_inclusions SET revoked_at=NOW() WHERE block_height>$1 AND revoked_at IS NULL")
            .bind(height).instrument("revoke_upgrade_authority").execute(self.storage).await?;
        Ok(())
    }

    pub async fn mark_via_bootstrap_version(&mut self,version:ProtocolSemanticVersion,expected:BaseSystemContractsHashes,verifier:L1VerifierConfig) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("bootstrap_protocol_origin").constraint_error(error))?;
        let result=sqlx::query("UPDATE protocol_patches pp SET via_bootstrap=TRUE FROM protocol_versions pv
            WHERE pv.id=pp.minor AND pp.minor=$1 AND pp.patch=$2 AND pp.immutable_payload IS NOT NULL
            AND pv.bootloader_code_hash=$3 AND pv.default_account_code_hash=$4
            AND pv.evm_emulator_code_hash IS NOT DISTINCT FROM $5 AND pp.snark_wrapper_vk_hash=$6
            AND pv.upgrade_tx_hash IS NULL")
            .bind(version.minor as i32).bind(version.patch.0 as i32).bind(expected.bootloader.as_bytes())
            .bind(expected.default_aa.as_bytes()).bind(expected.evm_emulator.as_ref().map(H256::as_bytes))
            .bind(verifier.snark_wrapper_vk_hash.as_bytes()).instrument("bootstrap_protocol_origin").execute(self.storage).await?;
        if result.rows_affected()!=1 {
            return Err(zksync_db_connection::instrument::Instrumented::new("bootstrap_protocol_origin").constraint_error(anyhow::anyhow!("bootstrap protocol content does not match its proved source")));
        }
        Ok(())
    }

    pub async fn record_via_upgrade_origin(
        &mut self,version:ProtocolSemanticVersion,identity:bitcoin::Txid,payload:&serde_json::Value,
        inclusion:via_reorg::chain::MessageRef<bitcoin::BlockHash,bitcoin::Txid>,
    ) -> DalResult<()> {
        use bitcoin::hashes::Hash;
        self.storage.via_indexer_dal().lock_btc_watch().await.map_err(|error| zksync_db_connection::instrument::Instrumented::new("record_upgrade_origin").constraint_error(error))?;
        let immutable=serde_json::json!({"version":version.to_string(),"message":payload});
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

#[cfg(test)]
mod bootstrap_tests {
    use super::*;
    use crate::ConnectionPool;

    #[tokio::test]
    async fn bootstrap_origin_requires_full_code_and_verifier_equivalence() {
        let pool=ConnectionPool::<Core>::test_pool().await;
        let mut connection=pool.connection().await.unwrap();
        let mut tx=connection.start_transaction().await.unwrap();
        let original=ProtocolVersion::default();
        tx.protocol_versions_dal().save_protocol_version_with_tx(&original).await.unwrap();
        for field in 0..4 {
            let mut hashes=original.base_system_contracts_hashes;
            let mut verifier=original.l1_verifier_config;
            match field {
                0=>hashes.bootloader=H256::repeat_byte(1),
                1=>hashes.default_aa=H256::repeat_byte(2),
                2=>hashes.evm_emulator=Some(H256::repeat_byte(3)),
                _=>verifier.snark_wrapper_vk_hash=H256::repeat_byte(4),
            }
            assert!(tx.protocol_versions_dal().mark_via_bootstrap_version(original.version,hashes,verifier).await.is_err());
            let classified:bool=sqlx::query_scalar("SELECT via_bootstrap FROM protocol_patches")
                .fetch_one(tx.conn()).await.unwrap();
            assert!(!classified);
        }
        tx.protocol_versions_dal().mark_via_bootstrap_version(original.version,
            original.base_system_contracts_hashes,original.l1_verifier_config).await.unwrap();
        let classified:bool=sqlx::query_scalar("SELECT via_bootstrap FROM protocol_patches")
            .fetch_one(tx.conn()).await.unwrap();
        assert!(classified);
    }
}
