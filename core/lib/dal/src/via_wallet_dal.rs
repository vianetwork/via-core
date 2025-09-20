use std::collections::HashMap;

use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::via_wallet::SystemWalletsDetails;

use crate::Core;

pub struct ViaWalletDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaWalletDal<'_, '_> {
    /// Inserts a new set of wallets.
    pub async fn insert_wallets(
        &mut self,
        wallets_details: &SystemWalletsDetails,
    ) -> DalResult<()> {
        let mut transaction = self.storage.start_transaction().await?;

        for (role, role_info) in wallets_details.0.clone() {
            // Join all addresses into a single comma-separated string
            let addresses_str = role_info
                .addresses
                .iter()
                .map(|addr| addr.to_string())
                .collect::<Vec<_>>()
                .join(",");

            sqlx::query!(
                r#"
                INSERT INTO
                via_wallets (role, address, tx_hash)
                VALUES
                ($1, $2, $3)
                ON CONFLICT (tx_hash, address, role) DO NOTHING
                "#,
                role.to_string(),
                addresses_str,
                role_info.txid.to_string(),
            )
            .instrument("insert_wallet")
            .report_latency()
            .execute(&mut transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(())
    }

    /// Fetch the latest system wallets from the DB (raw data).
    pub async fn get_system_wallets_raw(&mut self) -> DalResult<Option<HashMap<String, String>>> {
        let rows = sqlx::query!(
            r#"
            SELECT DISTINCT
            ON (ROLE)
                ROLE,
                ADDRESS
            FROM
                VIA_WALLETS
            ORDER BY
                ROLE,
                CREATED_AT DESC
            "#
        )
        .instrument("get_system_wallets_raw")
        .report_latency()
        .fetch_all(self.storage)
        .await?;

        if rows.is_empty() {
            return Ok(None);
        }

        let mut wallets = HashMap::new();
        for row in rows {
            // role and address are Strings
            wallets.insert(row.role, row.address);
        }

        Ok(Some(wallets))
    }
}
