// jscpd:ignore-start Independent Core and Verifier schemas own their wallet evidence and transactions.
use std::collections::HashMap;

use bitcoin::{hashes::Hash, BlockHash, Txid};
use serde_json::json;
use sqlx::Row;
use via_reorg::chain::MessageRef;
use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::via_wallet::SystemWalletsDetails;

use crate::{Verifier, VerifierDal};

pub struct ViaWalletDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

impl ViaWalletDal<'_, '_> {
    /// A configured bootstrap is classified separately and has no fabricated Bitcoin inclusion.
    pub async fn insert_bootstrap_wallets(
        &mut self,
        wallets: &SystemWalletsDetails,
        baseline_height: i64,
    ) -> DalResult<()> {
        let mut tx = self.storage.start_transaction().await?;
        tx.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("insert_bootstrap_wallets")
                .constraint_error(error)
        })?;
        let legacy: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM via_wallets WHERE origin_kind='legacy')",
        )
        .instrument("unproved_bootstrap_origins")
        .fetch_one(&mut tx)
        .await?;
        if legacy {
            return Err(Instrumented::new("bootstrap_wallets").constraint_error(anyhow::anyhow!(
                "unproved legacy wallet origins require stopped semantic migration"
            )));
        }
        for (role, info) in &wallets.0 {
            let address =
                info.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
            let payload = json!({"role":role.to_string(), "addresses":address, "origin":"bootstrap", "baseline":baseline_height});
            sqlx::query("INSERT INTO via_wallets(role,address,tx_hash,l1_block_number,origin_kind,immutable_payload)
                VALUES($1,$2,$3,$4,'bootstrap',$5)
                ON CONFLICT (tx_hash,message_index,role) DO UPDATE
                SET immutable_payload=via_btc_immutable_payload(via_wallets.immutable_payload,EXCLUDED.immutable_payload)")
                .bind(role.to_string()).bind(address).bind(info.txid.to_string()).bind(baseline_height).bind(payload)
                .instrument("insert_bootstrap_wallets").execute(&mut tx).await?;
        }
        tx.commit().await
    }

    pub async fn record_wallet_update(
        &mut self,
        wallets: &SystemWalletsDetails,
        inclusion: MessageRef<BlockHash, Txid>,
        payload: &serde_json::Value,
        decision: &str,
    ) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("record_wallet_update")
                .constraint_error(error)
        })?;
        if !self.storage.in_transaction() {
            return Err(Instrumented::new("record_wallet_update").constraint_error(
                anyhow::anyhow!("wallet inclusion requires an effects transaction"),
            ));
        }
        for (role, info) in &wallets.0 {
            let address =
                info.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
            self.record_wallet_role(*role, &address, info.txid, inclusion, payload, decision)
                .await?;
        }
        Ok(())
    }

    pub async fn record_wallet_rejection(
        &mut self,
        identity: Txid,
        roles: &[zksync_types::via_wallet::WalletRole],
        inclusion: MessageRef<BlockHash, Txid>,
        payload: &serde_json::Value,
    ) -> DalResult<()> {
        for role in roles {
            self.record_wallet_role(*role, "", identity, inclusion, payload, "bad_update").await?;
        }
        Ok(())
    }

    async fn record_wallet_role(
        &mut self,
        role: zksync_types::via_wallet::WalletRole,
        address: &str,
        identity: Txid,
        inclusion: MessageRef<BlockHash, Txid>,
        payload: &serde_json::Value,
        decision: &str,
    ) -> DalResult<()> {
        self.storage
            .via_indexer_dal()
            .lock_source_writer()
            .await
            .map_err(|error| Instrumented::new("record_wallet_role").constraint_error(error))?;
        let immutable = json!({"role":role.to_string(), "addresses":address,"origin":"bitcoin","message":payload});
        let wallet_id: i64 = sqlx::query_scalar("INSERT INTO via_wallets(role,address,tx_hash,l1_block_number,origin_kind,message_index,immutable_payload)
                VALUES($1,$2,$3,0,'bitcoin',$4,$5)
                ON CONFLICT (tx_hash,message_index,role) DO UPDATE
                SET immutable_payload=via_btc_immutable_payload(via_wallets.immutable_payload,EXCLUDED.immutable_payload)
                RETURNING id")
                .bind(role.to_string()).bind(address).bind(identity.to_string())
                .bind(i64::from(inclusion.message_index)).bind(immutable)
                .instrument("wallet_evidence").fetch_one(&mut *self.storage).await?;
        sqlx::query("INSERT INTO via_wallet_inclusions(wallet_id,block_hash,block_height,tx_index,message_index,decision,carrier_txid)
                VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (wallet_id,block_hash,tx_index,message_index) WHERE revoked_at IS NULL DO UPDATE
                SET decision=via_btc_immutable_payload(
                    jsonb_build_object('decision',via_wallet_inclusions.decision,'carrier_txid',via_wallet_inclusions.carrier_txid,'height',via_wallet_inclusions.block_height),
                    jsonb_build_object('decision',EXCLUDED.decision,'carrier_txid',EXCLUDED.carrier_txid,'height',EXCLUDED.block_height)) ->> 'decision'")
                .bind(wallet_id).bind(inclusion.block.block_hash.as_byte_array().as_slice())
                .bind(i64::from(inclusion.block.height)).bind(i64::from(inclusion.tx_index))
                .bind(i64::from(inclusion.message_index)).bind(decision).bind(inclusion.carrier_txid.as_byte_array().as_slice())
                .instrument("wallet_inclusion").execute(&mut *self.storage).await?;
        Ok(())
    }

    /// Configuration through this completed height governs the following block.
    pub async fn get_system_wallets_raw(
        &mut self,
        completed_height: i64,
    ) -> DalResult<Option<HashMap<String, String>>> {
        let legacy: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM via_wallets WHERE origin_kind='legacy')",
        )
        .instrument("unproved_wallet_origins")
        .fetch_one(&mut *self.storage)
        .await?;
        if legacy {
            return Err(Instrumented::new("get_system_wallets_raw").constraint_error(
                anyhow::anyhow!(
                    "unproved legacy wallet origins require stopped semantic migration"
                ),
            ));
        }
        let rows = sqlx::query("SELECT DISTINCT ON(w.role) w.role,w.address FROM via_wallets w
            LEFT JOIN via_wallet_inclusions i ON i.wallet_id=w.id AND i.revoked_at IS NULL AND i.decision='accepted'
            LEFT JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE (w.origin_kind='bootstrap' AND w.l1_block_number<=$1)
                OR (w.origin_kind='bitcoin' AND b.number IS NOT NULL AND i.block_height<=$1)
            ORDER BY w.role,(w.origin_kind='bitcoin') DESC,i.block_height DESC,i.tx_index DESC,i.message_index DESC")
            .bind(completed_height).try_map(|row: sqlx::postgres::PgRow| Ok((row.try_get::<String,_>("role")?,row.try_get::<String,_>("address")?)))
            .instrument("get_system_wallets_raw").fetch_all(self.storage).await?;
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(rows.into_iter().collect()))
    }

    /// Missing history is an error; the caller cannot supply a startup/latest set.
    pub async fn verifier_set_before(
        &mut self,
        proof_height: u32,
    ) -> DalResult<(i64, Vec<String>)> {
        let legacy: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM via_wallets WHERE origin_kind='legacy')",
        )
        .instrument("unproved_historical_set")
        .fetch_one(&mut *self.storage)
        .await?;
        if legacy {
            return Err(Instrumented::new("historical_verifier_set").constraint_error(
                anyhow::anyhow!(
                    "unproved legacy wallet origins require stopped semantic migration"
                ),
            ));
        }
        let row = sqlx::query("SELECT w.id,w.address FROM via_wallets w
            LEFT JOIN via_wallet_inclusions i ON i.wallet_id=w.id AND i.revoked_at IS NULL AND i.decision='accepted'
            LEFT JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
            WHERE w.role='Verifier' AND ((w.origin_kind='bootstrap' AND w.l1_block_number<$1)
                OR (w.origin_kind='bitcoin' AND b.number IS NOT NULL AND i.block_height<$1))
            ORDER BY (w.origin_kind='bitcoin') DESC,i.block_height DESC,i.tx_index DESC,i.message_index DESC LIMIT 1")
            .bind(i64::from(proof_height)).try_map(|row: sqlx::postgres::PgRow| {
                let addresses: String = row.try_get("address")?;
                Ok((row.try_get("id")?,addresses.split(',').map(str::to_owned).collect()))
            }).instrument("historical_verifier_set").fetch_optional(self.storage).await?;
        row.ok_or_else(|| {
            Instrumented::new("historical_verifier_set").constraint_error(anyhow::anyhow!(
                "missing historical verifier set before proof inclusion"
            ))
        })
    }

    /// Conditional revoke preserves evidence and is idempotent on a repeated recovery.
    /// Idea: Formance, https://github.com/formancehq/ledger/blob/3ab99b3ad28ffcc6034d94b4e3d831b823cce7a5/internal/storage/ledger/transactions.go#L183-L213
    pub async fn revoke_wallets_after(&mut self, height: i64) -> DalResult<u64> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("revoke_wallets_after")
                .constraint_error(error)
        })?;
        let result = sqlx::query("UPDATE via_wallet_inclusions SET revoked_at=NOW() WHERE block_height>$1 AND revoked_at IS NULL")
            .bind(height).instrument("revoke_wallet_inclusions").execute(self.storage).await?;
        Ok(result.rows_affected())
    }
}
// jscpd:ignore-end
