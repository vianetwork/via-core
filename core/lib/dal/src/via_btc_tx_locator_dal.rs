use sqlx::Row;
use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::Core;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViaBtcTxLocator {
    pub tx_id: String,
    pub l1_block_number: i64,
    pub l1_block_hash: String,
    pub tx_index: Option<i32>,
}

pub struct ViaBtcTxLocatorDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaBtcTxLocatorDal<'_, '_> {
    pub async fn insert_tx_locator(
        &mut self,
        tx_id: &str,
        l1_block_number: i64,
        l1_block_hash: &str,
        tx_index: Option<i32>,
    ) -> DalResult<()> {
        sqlx::query(
            r#"
            INSERT INTO via_btc_tx_locators
                (tx_id, l1_block_number, l1_block_hash, tx_index, created_at, updated_at)
            VALUES
                ($1, $2, $3, $4, NOW(), NOW())
            ON CONFLICT (tx_id) DO UPDATE SET
                l1_block_number = EXCLUDED.l1_block_number,
                l1_block_hash = EXCLUDED.l1_block_hash,
                tx_index = EXCLUDED.tx_index,
                updated_at = NOW()
            "#,
        )
        .bind(tx_id)
        .bind(l1_block_number)
        .bind(l1_block_hash)
        .bind(tx_index)
        .instrument("insert_tx_locator")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn get_tx_locator(&mut self, tx_id: &str) -> DalResult<Option<ViaBtcTxLocator>> {
        let row = sqlx::query(
            r#"
            SELECT tx_id, l1_block_number, l1_block_hash, tx_index
            FROM via_btc_tx_locators
            WHERE tx_id = $1
            "#,
        )
        .bind(tx_id)
        .instrument("get_tx_locator")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(row.map(|row| ViaBtcTxLocator {
            tx_id: row.get("tx_id"),
            l1_block_number: row.get("l1_block_number"),
            l1_block_hash: row.get("l1_block_hash"),
            tx_index: row.get("tx_index"),
        }))
    }

    pub async fn delete_tx_locators_after(&mut self, l1_block_number: i64) -> DalResult<()> {
        sqlx::query(
            r#"
            DELETE FROM via_btc_tx_locators
            WHERE l1_block_number > $1
            "#,
        )
        .bind(l1_block_number)
        .instrument("delete_tx_locators_after")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }
}
