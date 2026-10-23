use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};

use crate::{Verifier, VerifierDal};

pub struct ViaL1BlockDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

// jscpd:ignore-start Legacy metadata and canonical SQL stay under each concrete role’s transaction / source owner.
impl ViaL1BlockDal<'_, '_> {
    /// A canonical index entry must reference already retained, checked ancestry.
    pub async fn insert_l1_block(&mut self, number: i64, hash: String) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("insert_l1_block")
                .constraint_error(error)
        })?;
        use bitcoin::hashes::Hash;
        let hash: bitcoin::BlockHash = hash.parse().map_err(|err| {
            zksync_db_connection::instrument::Instrumented::new("insert_l1_block")
                .arg_error("hash", err)
        })?;
        sqlx::query("INSERT INTO via_l1_blocks(number,hash) VALUES($1,$2)
            ON CONFLICT(number) DO UPDATE SET hash=CASE WHEN via_l1_blocks.hash=EXCLUDED.hash THEN via_l1_blocks.hash ELSE NULL END")
            .bind(number).bind(hash.as_byte_array().as_slice())
            .instrument("insert_l1_block").execute(self.storage).await?;
        Ok(())
    }

    pub async fn get_last_l1_block(&mut self) -> DalResult<Option<(i64, String)>> {
        use bitcoin::hashes::Hash;
        use sqlx::Row;
        sqlx::query("SELECT number,hash FROM via_l1_blocks ORDER BY number DESC LIMIT 1")
            .try_map(|row: sqlx::postgres::PgRow| {
                let bytes: Vec<u8> = row.try_get("hash")?;
                let hash = bitcoin::BlockHash::from_slice(&bytes)
                    .map_err(|err| sqlx::Error::Decode(Box::new(err)))?;
                Ok((row.try_get("number")?, hash.to_string()))
            })
            .instrument("get_last_l1_block")
            .fetch_optional(self.storage)
            .await
    }

    pub async fn get_l1_block_hash(&mut self, height: i64) -> DalResult<Option<String>> {
        use bitcoin::hashes::Hash;
        use sqlx::Row;
        sqlx::query("SELECT hash FROM via_l1_blocks WHERE number=$1")
            .bind(height)
            .try_map(|row: sqlx::postgres::PgRow| {
                let bytes: Vec<u8> = row.try_get("hash")?;
                bitcoin::BlockHash::from_slice(&bytes)
                    .map(|hash| hash.to_string())
                    .map_err(|err| sqlx::Error::Decode(Box::new(err)))
            })
            .instrument("get_l1_block_hash")
            .fetch_optional(self.storage)
            .await
    }

    /// Delete L1 blocks.
    pub async fn delete_l1_blocks(&mut self, l1_block_number: i64) -> DalResult<()> {
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("delete_l1_blocks")
                .constraint_error(error)
        })?;
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
        self.storage.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("delete_l1_reorg")
                .constraint_error(error)
        })?;
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

    pub async fn list_l1_blocks(
        &mut self,
        height: i64,
        limit: i64,
    ) -> DalResult<Vec<(i64, String)>> {
        use bitcoin::hashes::Hash;
        use sqlx::Row;
        sqlx::query(
            "SELECT number,hash FROM via_l1_blocks WHERE number>=$1 ORDER BY number LIMIT $2",
        )
        .bind(height)
        .bind(limit)
        .try_map(|row: sqlx::postgres::PgRow| {
            let bytes: Vec<u8> = row.try_get("hash")?;
            let hash = bitcoin::BlockHash::from_slice(&bytes)
                .map_err(|err| sqlx::Error::Decode(Box::new(err)))?;
            Ok((row.try_get("number")?, hash.to_string()))
        })
        .instrument("list_l1_blocks")
        .fetch_all(self.storage)
        .await
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
        let mut tx = self.storage.start_transaction().await?;
        tx.via_indexer_dal().lock_source_writer().await.map_err(|error| {
            zksync_db_connection::instrument::Instrumented::new("insert_reorg_metadata")
                .constraint_error(error)
        })?;
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
        .execute(&mut tx)
        .await?;

        tx.commit().await
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
// jscpd:ignore-end

// jscpd:ignore-start Independent database adapters retain concrete SQL transaction ownership.
impl ViaL1BlockDal<'_, '_> {
    /// Persist a prepared immutable header; this never updates the canonical index.
    // Hash-keyed inclusion follows NBXplorer; disconnected ancestry remains addressable.
    // https://github.com/btcpayserver/NBXplorer/blob/b691999b14eb2a06547ffa708c11574fa323d769/NBXplorer/DBScripts/FullSchema.sql
    pub async fn retain_header(
        &mut self,
        header: via_reorg::chain::Header<bitcoin::BlockHash>,
    ) -> anyhow::Result<()> {
        use bitcoin::hashes::Hash;
        anyhow::ensure!(
            self.storage.in_transaction(),
            "header retention requires an effects transaction"
        );
        let accepted: Option<Vec<u8>> = sqlx::query_scalar(
            "INSERT INTO via_btc_headers(hash,height,parent_hash) VALUES ($1,$2,$3) \
             ON CONFLICT(hash) DO UPDATE SET hash=EXCLUDED.hash \
             WHERE via_btc_headers.height=EXCLUDED.height AND via_btc_headers.parent_hash=EXCLUDED.parent_hash RETURNING hash",
        )
        .bind(header.block_hash.as_byte_array().as_slice()).bind(i64::from(header.height))
        .bind(header.parent_hash.as_byte_array().as_slice())
        .instrument("retain_bitcoin_header").fetch_optional(self.storage).await?;
        anyhow::ensure!(accepted.is_some(), "immutable Bitcoin header payload conflict");
        Ok(())
    }

    pub async fn accept_header(
        &mut self,
        header: via_reorg::chain::Header<bitcoin::BlockHash>,
    ) -> anyhow::Result<()> {
        self.retain_header(header).await?;
        self.insert_l1_block(i64::from(header.height), header.block_hash.to_string()).await?;
        Ok(())
    }

    pub async fn retained_header(
        &mut self,
        hash: bitcoin::BlockHash,
    ) -> anyhow::Result<Option<via_reorg::chain::Header<bitcoin::BlockHash>>> {
        use bitcoin::hashes::Hash;
        use sqlx::Row;
        let row: Option<(i64, Vec<u8>)> =
            sqlx::query("SELECT height,parent_hash FROM via_btc_headers WHERE hash=$1")
                .bind(hash.as_byte_array().as_slice())
                .try_map(|row: sqlx::postgres::PgRow| {
                    Ok((row.try_get("height")?, row.try_get("parent_hash")?))
                })
                .instrument("retained_bitcoin_header")
                .fetch_optional(self.storage)
                .await?;
        row.map(|(height, parent)| {
            Ok(via_reorg::chain::Header {
                block_hash: hash,
                height: u32::try_from(height)?,
                parent_hash: bitcoin::BlockHash::from_slice(&parent)?,
            })
        })
        .transpose()
    }

    /// Legacy canonical rows are parsed as Bitcoin display hashes, never reversed as an SQL string.
    /// Missing retained ancestry remains unknown until stopped migration proves it.
    pub async fn canonical_header(
        &mut self,
        height: u32,
    ) -> anyhow::Result<Option<via_reorg::chain::Header<bitcoin::BlockHash>>> {
        use std::str::FromStr;
        let header = match self.get_l1_block_hash(i64::from(height)).await? {
            Some(hash) => self.retained_header(bitcoin::BlockHash::from_str(&hash)?).await?,
            None => return Ok(None),
        };
        if let Some(header) = &header {
            anyhow::ensure!(
                header.height == height,
                "legacy canonical Bitcoin height {height} references retained height {}",
                header.height
            );
        }
        Ok(header)
    }
}
// jscpd:ignore-end
