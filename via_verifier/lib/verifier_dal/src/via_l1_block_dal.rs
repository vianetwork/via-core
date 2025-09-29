use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::Verifier;

pub struct ViaL1BlockDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

impl ViaL1BlockDal<'_, '_> {
    /// Inserts a new l1 block.
    pub async fn insert_l1_block(&mut self, number: i64, hash: String) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_l1_blocks (number, hash)
            VALUES
                ($1, $2)
            ON CONFLICT (number) DO NOTHING
            "#,
            number,
            hash
        )
        .instrument("insert_l1_block")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }

    /// Fetch the latest inserted block number.
    pub async fn get_last_l1_block(&mut self) -> DalResult<Option<(i64, String)>> {
        let record = sqlx::query!(
            r#"
            SELECT
                number,
                hash
            FROM
                via_l1_blocks
            ORDER BY
                number DESC
            LIMIT
                1
            "#
        )
        .instrument("get_last_l1_block")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(record.map(|r| (r.number, r.hash)))
    }

    /// Fetch the block number and hash.
    pub async fn get_l1_block_hash(&mut self, block_height: i64) -> DalResult<Option<String>> {
        let record = sqlx::query!(
            r#"
            SELECT
                hash
            FROM
                via_l1_blocks
            WHERE
                number = $1
            ORDER BY
                number DESC
            LIMIT
                1
            "#,
            block_height
        )
        .instrument("get_l1_block_hash")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(record.map(|r| (r.hash)))
    }

    /// Delete L1 blocks.
    pub async fn delete_l1_blocks(&mut self, l1_block_number: i64) -> DalResult<()> {
        sqlx::query!(
            r#"
            DELETE FROM via_l1_blocks
            WHERE
                number > $1
            "#,
            l1_block_number
        )
        .instrument("delete_l1_blocks")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }

    /// Delete reorg.
    pub async fn delete_l1_reorg(&mut self, l1_block_number: i64) -> DalResult<()> {
        sqlx::query!(
            r#"
            DELETE FROM via_l1_reorg
            WHERE
                l1_block_number > $1
            "#,
            l1_block_number
        )
        .instrument("delete_l1_reorg")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }

    /// Fetch a list of l1 blocks (number, hash).
    pub async fn list_l1_blocks(
        &mut self,
        block_height: i64,
        limit: i64,
    ) -> DalResult<Vec<(i64, String)>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                number,
                hash
            FROM
                via_l1_blocks
            WHERE
                number >= $1
            ORDER BY
                number ASC
            LIMIT
                $2
            "#,
            block_height,
            limit,
        )
        .instrument("list_l1_blocks")
        .report_latency()
        .fetch_all(self.storage)
        .await?;

        let blocks = rows
            .into_iter()
            .map(|row| (row.number, row.hash))
            .collect::<Vec<(i64, String)>>();

        Ok(blocks)
    }

    pub async fn list_votable_transactions(
        &mut self,
        l1_batch_number: i64,
    ) -> DalResult<Option<i64>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                MIN(l1_batch_number) AS l1_batch_number
            FROM
                via_votable_transactions
            WHERE
                l1_batch_number >= $1
            "#,
            l1_batch_number,
        )
        .instrument("list_votable_transactions")
        .report_latency()
        .fetch_one(self.storage)
        .await?;

        Ok(rows.l1_batch_number)
    }

    pub async fn insert_reorg_metadata(
        &mut self,
        l1_block_number: i64,
        l1_batch_number: i64,
    ) -> DalResult<()> {
        sqlx::query!(
            r#"
            INSERT INTO
                via_l1_reorg (l1_block_number, l1_batch_number)
            VALUES
                ($1, $2)
            ON CONFLICT (l1_block_number) DO NOTHING
            "#,
            l1_block_number,
            l1_batch_number,
        )
        .instrument("insert_reorg_metadata")
        .report_latency()
        .execute(self.storage)
        .await?;

        Ok(())
    }

    pub async fn has_reorg_in_progress(&mut self) -> DalResult<Option<(i64, i64)>> {
        let record = sqlx::query!(
            r#"
            SELECT
                l1_block_number,
                l1_batch_number
            FROM
                via_l1_reorg
            ORDER BY
                l1_block_number ASC
            LIMIT
                1
            "#
        )
        .instrument("has_reorg_in_progress")
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(record.map(|row| (row.l1_block_number, row.l1_batch_number)))
    }
}
