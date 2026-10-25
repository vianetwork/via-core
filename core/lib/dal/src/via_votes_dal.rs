use via_btc_client::indexer::replay::{Included, ProofInput};
use sqlx::Row;
use zksync_db_connection::{connection::Connection, instrument::InstrumentExt};

use crate::Core;

// Exact pre-reset outbound receipt identity supplies the main-node Q26 exception.

// Sender history keeps raw Txid bytes at this boundary.

pub struct ViaVotesDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Core>,
}

impl ViaVotesDal<'_, '_> {
    pub async fn save_reader_proof(
        &mut self,
        proof: &Included<ProofInput>,
        header_id: i64,
    ) -> anyhow::Result<()> {
        use bitcoin::hashes::Hash;
        via_btc_storage::ReaderDal::new(self.storage)
            .check_proof_source(proof, header_id)
            .await?;
        let origin = &proof.origin;
        let raw = origin.txid.to_byte_array();
        let parent = &proof.payload.input.pubdata;
        let inserted = sqlx::query_scalar::<_, i64>("INSERT INTO via_btc_proofs(l1_batch_number,proof_reveal_tx_id,pubdata_reveal_tx_id,l1_block_number,block_hash,
            transaction_index,message_index,payload,proof_header_id)
            SELECT $1,$2,$3,h.number,h.hash,$6,$7,$8,h.id FROM via_btc_headers h WHERE h.id=$9 AND h.number=$4 AND h.hash=$5
              AND NOT EXISTS(SELECT 1 FROM via_btc_proofs old JOIN via_btc_headers canonical
                  ON canonical.id=old.proof_header_id WHERE old.proof_reveal_tx_id=$2 AND old.block_hash<>h.hash)
              AND NOT EXISTS(SELECT 1 FROM via_btc_proofs old JOIN via_btc_headers canonical
                  ON canonical.id=old.proof_header_id WHERE canonical.id=h.id
                  AND old.transaction_index=$6 AND old.message_index=$7 AND old.proof_reveal_tx_id<>$2)
            ON CONFLICT DO NOTHING RETURNING id")
            .bind(i64::from(parent.payload.input.l1_batch_index.0)).bind(raw.as_slice()).bind(parent.origin.txid.to_byte_array().as_slice())
            .bind(i64::from(origin.height)).bind(origin.hash.to_string()).bind(i32::try_from(origin.transaction_index)?)
            .bind(i32::try_from(origin.message_index)?).bind(serde_json::to_value(proof)?).bind(header_id)
            .instrument("main_reader_save_proof").fetch_optional(self.storage).await?;
        if inserted.is_none() {
            let saved = sqlx::query("SELECT p.payload,p.block_hash FROM via_btc_proofs p
                WHERE p.proof_reveal_tx_id=$1 AND (p.block_hash=$2 OR EXISTS(
                    SELECT 1 FROM via_btc_headers h WHERE h.number=p.l1_block_number AND h.hash=p.block_hash))")
                .bind(raw.as_slice()).bind(origin.hash.to_string()).instrument("main_proof_replay").fetch_optional(self.storage).await?
                .ok_or(via_btc_client::indexer::replay::ReplayConflict { field: "proof_source_key" })?;
            via_btc_client::indexer::replay::compare_fields([(
                "duplicate_canonical_inclusion",
                saved.try_get::<String, _>("block_hash")? == origin.hash.to_string(),
            )])?;
            proof.compare(&serde_json::from_value(saved.try_get("payload")?)?)?;
        }
        Ok(())
    }
}
