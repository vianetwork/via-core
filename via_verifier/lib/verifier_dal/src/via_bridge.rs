use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::Verifier;

#[derive(Debug)]
pub struct ViaBridgeDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Verifier>,
}

impl ViaBridgeDal<'_, '_> {
    pub async fn insert_bridge_txs(
        &mut self,
        votable_tx_id: i64,
        unsigned_bridge_txs: &[Vec<u8>],
    ) -> DalResult<()> {
        let mut db_transaction = self.storage.start_transaction().await?;

        for (index, data) in unsigned_bridge_txs.iter().enumerate() {
            sqlx::query!(
                r#"
                INSERT INTO
                    via_bridge_tx (votable_tx_id, data, INDEX)
                VALUES
                    ($1, $2, $3)
                "#,
                votable_tx_id,
                data,
                index as i64
            )
            .instrument("insert_bridge_txs")
            .execute(&mut db_transaction)
            .await?;
        }

        db_transaction.commit().await?;

        Ok(())
    }

    pub async fn insert_bridge_tx(
        &mut self,
        votable_tx_id: i64,
        hash: Option<&[u8]>,
        data: Option<&[u8]>,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_bridge_tx (votable_tx_id, hash, data)
            VALUES
                ($1, $2, $3)
            "#,
            votable_tx_id,
            hash,
            data
        )
        .instrument("insert_bridge_tx")
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn update_bridge_tx(
        &mut self,
        votable_tx_id: i64,
        index: i64,
        hash: &[u8],
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            UPDATE via_bridge_tx
            SET
                hash = $3,
                updated_at = NOW()
            WHERE
                votable_tx_id = $1
                AND INDEX = $2
            "#,
            votable_tx_id,
            index,
            hash
        )
        .instrument("update_bridge_tx")
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn get_vote_transaction_bridge_txs(
        &mut self,
        votable_tx_id: i64,
    ) -> DalResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                data,
                hash
            FROM
                via_bridge_tx
            WHERE
                votable_tx_id = $1
            ORDER BY
                id ASC
            "#,
            votable_tx_id
        )
        .instrument("get_vote_transaction_bridge_txs")
        .fetch_all(self.storage)
        .await?;

        let bridge_txs: Vec<(Vec<u8>, Vec<u8>)> = rows
            .into_iter()
            .map(|row| (row.data.unwrap_or_default(), row.hash.unwrap_or_default()))
            .collect();

        Ok(bridge_txs) // will be empty Vec if no rows or all NULLs
    }

    pub async fn list_bridge_txs_not_yet_processed(&mut self) -> DalResult<Vec<(i64, i64)>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                id,
                votable_tx_id
            FROM
                via_bridge_tx
            WHERE
                hash IS NULL
            ORDER BY
                id ASC
            "#
        )
        .instrument("list_bridge_txs_not_yet_processed")
        .fetch_all(self.storage)
        .await?;

        let bridge_txs = rows
            .into_iter()
            .filter_map(|row| {
                row.votable_tx_id
                    .map(|votable_tx_id| (votable_tx_id, row.id))
            })
            .collect();

        Ok(bridge_txs)
    }
}
