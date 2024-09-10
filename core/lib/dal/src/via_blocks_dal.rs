use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::{
    btc_block::ViaBtcBlockDetails, btc_inscription_operations::ViaBtcInscriptionRequestType,
    L1BatchNumber,
};

pub use crate::models::storage_block::{L1BatchMetadataError, L1BatchWithOptionalMetadata};
use crate::{models::storage_btc_block::ViaBtcStorageBlockDetails, Core};

#[derive(Debug)]
pub struct ViaBlocksDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaBlocksDal<'_, '_> {
    pub async fn set_inscription_request_id(
        &mut self,
        batch_number: L1BatchNumber,
        inscription_request_id: i64,
        inscription_request: ViaBtcInscriptionRequestType,
    ) -> DalResult<()> {
        match inscription_request {
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain => {
                let instrumentation = Instrumented::new("set_eth_tx_id#commit")
                    .with_arg("batch_number", &batch_number)
                    .with_arg("inscription_request_id", &inscription_request_id);

                let query = sqlx::query!(
                    r#"
                    UPDATE l1_batches
                    SET
                        eth_commit_tx_id = $1,
                        updated_at = NOW()
                    WHERE
                        number = $2
                        AND eth_commit_tx_id IS NULL
                    "#,
                    inscription_request_id as i32,
                    i64::from(batch_number.0),
                );
                let result = instrumentation
                    .clone()
                    .with(query)
                    .execute(self.storage)
                    .await?;

                if result.rows_affected() == 0 {
                    let err = instrumentation.constraint_error(anyhow::anyhow!(
                        "Update eth_commit_tx_id that is is not null is not allowed"
                    ));
                    return Err(err);
                }
                Ok(())
            }
            ViaBtcInscriptionRequestType::CommitProofOnchain => {
                let instrumentation = Instrumented::new("set_eth_tx_id#prove")
                    .with_arg("batch_number", &batch_number)
                    .with_arg("inscription_request_id", &inscription_request_id);
                let query = sqlx::query!(
                    r#"
                    UPDATE l1_batches
                    SET
                        eth_prove_tx_id = $1,
                        updated_at = NOW()
                    WHERE
                        number = $2
                        AND eth_prove_tx_id IS NULL
                    "#,
                    inscription_request_id as i32,
                    i64::from(batch_number.0),
                );

                let result = instrumentation
                    .clone()
                    .with(query)
                    .execute(self.storage)
                    .await?;

                if result.rows_affected() == 0 {
                    let err = instrumentation.constraint_error(anyhow::anyhow!(
                        "Update eth_prove_tx_id that is is not null is not allowed"
                    ));
                    return Err(err);
                }
                Ok(())
            }
        }
    }

    pub async fn get_inscription_commit_tx_id(
        &mut self,
        l1_batch_number: L1BatchNumber,
    ) -> DalResult<Option<u64>> {
        let row = sqlx::query!(
            r#"
            SELECT
                eth_commit_tx_id
            FROM
                l1_batches
            WHERE
                number = $1
            "#,
            i64::from(l1_batch_number.0)
        )
        .instrument("get_inscription_commit_tx_id")
        .with_arg("l1_batch_number", &l1_batch_number)
        .fetch_optional(self.storage)
        .await?;

        Ok(row.and_then(|row| row.eth_commit_tx_id.map(|n| n as u64)))
    }

    pub async fn get_block_commit_details(
        &mut self,
        l1_block_number: i64,
    ) -> anyhow::Result<Option<ViaBtcBlockDetails>> {
        let batch_details = sqlx::query_as!(
            ViaBtcStorageBlockDetails,
            r#"
            SELECT
                l1_batches.number,
                l1_batches.hash,
                via_btc_inscriptions_request_history.commit_tx_id,
                via_btc_inscriptions_request_history.reveal_tx_id,
                via_btc_inscriptions_request_history.inscription_request_context_id
            FROM
                l1_batches
                LEFT JOIN via_btc_inscriptions_request ON l1_batches.eth_commit_tx_id = via_btc_inscriptions_request.id
                LEFT JOIN via_btc_inscriptions_request_history ON via_btc_inscriptions_request.id = via_btc_inscriptions_request_history.inscription_request_id
            WHERE
                eth_commit_tx_id IS NOT NULL
                AND eth_prove_tx_id IS NULL
                AND number = $1
            "#,
            l1_block_number
        )
        .fetch_optional(self.storage.conn())
        .await?;

        Ok(batch_details.map(|batch_details| batch_details.into()))
    }
}
