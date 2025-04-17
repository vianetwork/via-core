use zksync_db_connection::{
    connection::Connection,
    error::DalResult,
    instrument::{InstrumentExt, Instrumented},
};
use zksync_types::via_verifier_btc_inscription_operations::ViaVerifierBtcInscriptionRequestType;

use crate::Verifier;

#[derive(Debug)]
pub struct ViaBlocksDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Verifier>,
}

impl ViaBlocksDal<'_, '_> {
    pub async fn insert_vote_l1_batch_inscription_request_id(
        &mut self,
        votable_transaction_id: i64,
        inscription_request_id: i64,
        inscription_request: ViaVerifierBtcInscriptionRequestType,
    ) -> DalResult<()> {
        match inscription_request {
            ViaVerifierBtcInscriptionRequestType::VoteOnchain => {
                let instrumentation = Instrumented::new("set_inscription_request_tx_id#commit")
                    .with_arg("votable_transaction_id", &votable_transaction_id)
                    .with_arg("inscription_request_id", &inscription_request_id);

                let query = sqlx::query!(
                    r#"
                    INSERT INTO
                        via_l1_batch_vote_inscription_request (votable_transaction_id, vote_l1_batch_inscription_id, created_at, updated_at)
                    VALUES
                        ($1, $2, NOW(), NOW())
                    ON CONFLICT DO NOTHING
                    "#,
                    votable_transaction_id,
                    inscription_request_id as i32,
                );
                let result = instrumentation
                    .clone()
                    .with(query)
                    .execute(self.storage)
                    .await?;

                if result.rows_affected() == 0 {
                    let err = instrumentation.constraint_error(anyhow::anyhow!(
                        "Failed to insert into 'via_l1_batch_vote_inscription_request': \
                        No rows were affected. This could be due to a conflict or invalid input values. \
                        votable_transaction_id: {:?}, inscription_request_id: {:?}",
                        votable_transaction_id,
                        inscription_request_id as i32
                    ));
                    return Err(err);
                }
                Ok(())
            }
        }
    }

    pub async fn check_vote_l1_batch_inscription_request_if_exists(
        &mut self,
        batch_number: i64,
    ) -> DalResult<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM via_l1_batch_vote_inscription_request
                WHERE votable_transaction_id = $1
            )
            "#,
            batch_number
        )
        .instrument("check_vote_l1_batch_inscription_request_id_exists")
        .fetch_one(self.storage)
        .await?;

        Ok(exists.unwrap_or(false))
    }

    pub async fn get_first_stuck_l1_batch_number_inscription_request(
        &mut self,
        delay_btc_blocks: u32,
        current_btc_blocks: u64,
    ) -> DalResult<u32> {
        let record = sqlx::query_scalar!(
            r#"
            SELECT 
                MIN(l1_batch_number) as l1_batch_number
            FROM
                via_votable_transactions
            LEFT JOIN
                via_l1_batch_vote_inscription_request
            ON
                via_votable_transactions.id = via_l1_batch_vote_inscription_request.votable_transaction_id
            LEFT JOIN
                via_btc_inscriptions_request_history
            ON
                inscription_request_id = vote_l1_batch_inscription_id
            WHERE
                sent_at_block + $1 < $2 
            "#,
            i64::from(delay_btc_blocks),
            current_btc_blocks as i64
        )
        .instrument("get_first_stuck_l1_batch_number_inscription_request")
        .fetch_one(self.storage)
        .await?;

        Ok(record.unwrap_or(0) as u32)
    }
}
