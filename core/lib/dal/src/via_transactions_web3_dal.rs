use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::{api::TransactionDetails, H256};

use crate::{models::via_storage_transaction::ViaStorageTransactionDetails, Core};

#[derive(Debug)]
pub struct ViaTransactionsWeb3Dal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaTransactionsWeb3Dal<'_, '_> {
    pub async fn get_transaction_details(
        &mut self,
        hash: H256,
    ) -> DalResult<Option<TransactionDetails>> {
        let row = sqlx::query_as!(
            ViaStorageTransactionDetails,
            r#"
            SELECT
                transactions.is_priority,
                transactions.initiator_address,
                transactions.gas_limit,
                transactions.gas_per_pubdata_limit,
                transactions.received_at,
                miniblocks.number AS "miniblock_number?",
                transactions.error,
                transactions.effective_gas_price,
                transactions.refunded_gas,
                commit_history.reveal_tx_id AS "commit_tx_hash?",
                proof_history.reveal_tx_id AS "prove_tx_hash?",
                bir.is_finalized
            FROM
                transactions
            LEFT JOIN miniblocks ON miniblocks.number = transactions.miniblock_number
            LEFT JOIN l1_batches ON l1_batches.number = miniblocks.l1_batch_number
            LEFT JOIN
                via_l1_batch_inscription_request AS bir
                ON (l1_batches.number = bir.l1_batch_number)
            LEFT JOIN
                via_btc_inscriptions_request commit_req
                ON bir.commit_l1_batch_inscription_id = commit_req.id
            LEFT JOIN
                via_btc_inscriptions_request proof_req
                ON bir.commit_proof_inscription_id = proof_req.id
            LEFT JOIN
                via_btc_inscriptions_request_history commit_history
                ON commit_req.confirmed_inscriptions_request_history_id = commit_history.id
            LEFT JOIN
                via_btc_inscriptions_request_history proof_history
                ON proof_req.confirmed_inscriptions_request_history_id = proof_history.id
            WHERE
                transactions.hash = $1
                AND transactions.data != '{}'::jsonb
            "#,
            // ^ Filter out transactions with pruned data, which would lead to potentially incomplete / bogus
            // transaction info.
            hash.as_bytes()
        )
        .instrument("get_transaction_details")
        .with_arg("hash", &hash)
        .fetch_optional(self.storage)
        .await?;

        Ok(row.map(Into::into))
    }
}
