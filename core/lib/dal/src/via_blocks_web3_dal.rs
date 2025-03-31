use zksync_db_connection::{connection::Connection, error::DalResult, instrument::InstrumentExt};
use zksync_types::{
    api::{BlockDetails, L1BatchDetails},
    L1BatchNumber, L2BlockNumber,
};

pub use crate::models::storage_block::{L1BatchMetadataError, L1BatchWithOptionalMetadata};
use crate::{
    models::storage_btc_block::{ViaStorageBlockDetails, ViaStorageL1BatchDetails},
    Core,
};

#[derive(Debug)]
pub struct ViaBlocksWeb3Dal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaBlocksWeb3Dal<'_, '_> {
    pub async fn get_l1_batch_details(
        &mut self,
        l1_batch_number: L1BatchNumber,
    ) -> DalResult<Option<L1BatchDetails>> {
        let l1_batch_details = sqlx::query_as!(
            ViaStorageL1BatchDetails,
            r#"
            WITH
                mb AS (
                    SELECT
                        l1_gas_price,
                        l2_fair_gas_price,
                        fair_pubdata_price
                    FROM
                        miniblocks
                    WHERE
                        l1_batch_number = $1
                    LIMIT
                        1
                )
            SELECT
                l1_batches.number,
                l1_batches.timestamp,
                l1_batches.l1_tx_count,
                l1_batches.l2_tx_count,
                l1_batches.hash AS "root_hash?",
                commit_history.reveal_tx_id AS "commit_tx_hash?",
                commit_history.confirmed_at AS "committed_at?",
                proof_history.reveal_tx_id AS "prove_tx_hash?",
                proof_history.confirmed_at AS "proven_at?",
                bir.is_finalized AS "is_finalized?",
                bir.updated_at AS "executed_at?",
                mb.l1_gas_price,
                mb.l2_fair_gas_price,
                mb.fair_pubdata_price,
                l1_batches.bootloader_code_hash,
                l1_batches.default_aa_code_hash
            FROM
                l1_batches
                INNER JOIN mb ON TRUE
                LEFT JOIN via_l1_batch_inscription_request AS bir ON (l1_batches.number = bir.l1_batch_number)
                LEFT JOIN via_btc_inscriptions_request commit_req ON bir.commit_l1_batch_inscription_id = commit_req.id
                LEFT JOIN via_btc_inscriptions_request proof_req ON bir.commit_proof_inscription_id = proof_req.id
                LEFT JOIN via_btc_inscriptions_request_history commit_history ON commit_req.confirmed_inscriptions_request_history_id = commit_history.id
                LEFT JOIN via_btc_inscriptions_request_history proof_history ON proof_req.confirmed_inscriptions_request_history_id = proof_history.id
            WHERE
                l1_batches.number = $1
                AND commit_history.confirmed_at IS NOT NULL
                AND proof_history.confirmed_at IS NOT NULL
            "#,
            i64::from(l1_batch_number.0)
        )
        .instrument("get_l1_batch_details")
        .with_arg("l1_batch_number", &l1_batch_number)
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(l1_batch_details.map(Into::into))
    }

    pub async fn get_block_details(
        &mut self,
        block_number: L2BlockNumber,
    ) -> DalResult<Option<BlockDetails>> {
        let storage_block_details = sqlx::query_as!(
            ViaStorageBlockDetails,
            r#"
            SELECT
                miniblocks.number,
                COALESCE(
                    miniblocks.l1_batch_number,
                    (
                        SELECT
                            (MAX(number) + 1)
                        FROM
                            l1_batches
                    )
                ) AS "l1_batch_number!",
                miniblocks.timestamp,
                miniblocks.l1_tx_count,
                miniblocks.l2_tx_count,
                miniblocks.hash AS "root_hash?",
                commit_history.reveal_tx_id AS "commit_tx_hash?",
                commit_history.confirmed_at AS "committed_at?",
                proof_history.reveal_tx_id AS "prove_tx_hash?",
                proof_history.confirmed_at AS "proven_at?",
                bir.is_finalized,
                bir.updated_at AS "executed_at?",
                miniblocks.l1_gas_price,
                miniblocks.l2_fair_gas_price,
                miniblocks.fair_pubdata_price,
                miniblocks.bootloader_code_hash,
                miniblocks.default_aa_code_hash,
                miniblocks.protocol_version,
                miniblocks.fee_account_address
            FROM
                miniblocks
                LEFT JOIN l1_batches ON miniblocks.l1_batch_number = l1_batches.number
                LEFT JOIN via_l1_batch_inscription_request AS bir ON (l1_batches.number = bir.l1_batch_number)
                LEFT JOIN via_btc_inscriptions_request commit_req ON bir.commit_l1_batch_inscription_id = commit_req.id
                LEFT JOIN via_btc_inscriptions_request proof_req ON bir.commit_proof_inscription_id = proof_req.id
                LEFT JOIN via_btc_inscriptions_request_history commit_history ON commit_req.confirmed_inscriptions_request_history_id = commit_history.id
                LEFT JOIN via_btc_inscriptions_request_history proof_history ON proof_req.confirmed_inscriptions_request_history_id = proof_history.id
            WHERE
                miniblocks.number = $1
            "#,
            i64::from(block_number.0)
        )
        .instrument("get_block_details")
        .with_arg("block_number", &block_number)
        .report_latency()
        .fetch_optional(self.storage)
        .await?;

        Ok(storage_block_details.map(Into::into))
    }
}
