use std::collections::HashMap;

use zksync_db_connection::{
    connection::{Connection, DbMarker},
    error::DalResult,
    instrument::InstrumentExt,
};
use zksync_types::via_wallet::SystemWalletsDetails;

use crate::{ReaderDal, WalletSource};

pub struct WalletDal<'c, 'a, DB: DbMarker> {
    storage: &'c mut Connection<'a, DB>,
}

impl<'c, 'a, DB: DbMarker> WalletDal<'c, 'a, DB> {
    pub fn new(storage: &'c mut Connection<'a, DB>) -> Self {
        Self { storage }
    }

    /// Inserts a new set of wallets.
    pub async fn insert_wallets(
        &mut self,
        wallets_details: &SystemWalletsDetails,
        l1_block_number: i64,
    ) -> anyhow::Result<()> {
        let mut transaction = self.storage.start_transaction().await?;
        ReaderDal::new(&mut transaction)
            .insert_wallets(wallets_details, WalletSource::Unbound(l1_block_number))
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Fetch the latest system wallets from the DB (raw data).
    pub async fn get_system_wallets_raw(
        &mut self,
        l1_block_number: i64,
    ) -> DalResult<Option<HashMap<String, String>>> {
        let rows = sqlx::query!(
            r#"
            SELECT DISTINCT
            ON (ROLE)
                ROLE,
                ADDRESS
            FROM
                VIA_WALLETS
            WHERE
                L1_BLOCK_NUMBER <= $1
            ORDER BY
                ROLE,
                CREATED_AT DESC
            "#,
            l1_block_number
        )
        .instrument("get_system_wallets_raw")
        .report_latency()
        .fetch_all(self.storage)
        .await?;

        if rows.is_empty() {
            return Ok(None);
        }

        let wallets = rows.into_iter().map(|row| (row.role, row.address)).collect();
        Ok(Some(wallets))
    }

    pub async fn delete_system_wallet(&mut self, l1_block_number: i64) -> DalResult<()> {
        sqlx::query_scalar!(
            r#"
            DELETE FROM via_wallets 
            WHERE l1_block_number > $1
            "#,
            l1_block_number
        )
        .instrument("delete_system_wallet")
        .execute(self.storage)
        .await?;

        Ok(())
    }
}
