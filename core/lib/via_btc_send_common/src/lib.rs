use anyhow::Context;
use async_trait::async_trait;
use bincode::serialize;
use bitcoin::{hashes::Hash, Txid};
use chrono::NaiveDateTime;
use via_btc_client::{inscriber::Inscriber, traits::Serializable, types::InscriptionMessage};

pub struct InscriptionResult {
    pub commit_tx_id_bytes: Vec<u8>,
    pub reveal_tx_id_bytes: Vec<u8>,
    pub commit_txid: Txid,
    pub reveal_txid: Txid,
    pub signed_commit_tx: Vec<u8>,
    pub signed_reveal_tx: Vec<u8>,
    pub actual_fees_sat: i64,
}

pub async fn inscribe_and_prepare(
    inscriber: &mut Inscriber,
    inscription_message: &[u8],
) -> anyhow::Result<InscriptionResult> {
    let input = InscriptionMessage::from_bytes(inscription_message);
    let inscribe_info = inscriber.inscribe(input).await?;

    let signed_commit_tx = serialize(&inscribe_info.final_commit_tx.tx)
        .with_context(|| "Error serializing the commit tx")?;
    let signed_reveal_tx = serialize(&inscribe_info.final_reveal_tx.tx)
        .with_context(|| "Error serializing the reveal tx")?;

    let actual_fees = inscribe_info.reveal_tx_output_info._reveal_fee
        + inscribe_info.commit_tx_output_info.commit_tx_fee;

    Ok(InscriptionResult {
        commit_tx_id_bytes: inscribe_info
            .final_commit_tx
            .txid
            .as_raw_hash()
            .to_byte_array()
            .to_vec(),
        reveal_tx_id_bytes: inscribe_info
            .final_reveal_tx
            .txid
            .as_raw_hash()
            .to_byte_array()
            .to_vec(),
        commit_txid: inscribe_info.final_commit_tx.txid,
        reveal_txid: inscribe_info.final_reveal_tx.txid,
        signed_commit_tx,
        signed_reveal_tx,
        actual_fees_sat: actual_fees.to_sat() as i64,
    })
}

/// Common types for inscription requests and history across sequencer and verifier
#[derive(Clone, Debug)]
pub struct CommonInscriptionRequest {
    pub id: i64,
    pub request_type: String,
    pub inscription_message: Option<Vec<u8>>,
    pub predicted_fee: Option<i64>,
    pub confirmed_inscriptions_request_history_id: Option<i64>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Clone, Debug)]
pub struct CommonInscriptionHistory {
    pub id: i64,
    pub commit_txid: Txid,
    pub reveal_txid: Txid,
    pub inscription_request_id: i64,
    pub signed_commit_tx: Option<Vec<u8>>,
    pub signed_reveal_tx: Option<Vec<u8>>,
    pub actual_fees: i64,
    pub sent_at_block: i64,
    pub confirmed_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

/// Input for creating inscription history
pub struct InscriptionHistoryInput<'a> {
    pub commit_txid: Txid,
    pub reveal_txid: Txid,
    pub signed_commit_tx: &'a [u8],
    pub signed_reveal_tx: &'a [u8],
    pub actual_fees_sat: i64,
    pub sent_at_block: i64,
}

/// Common DAL trait for BTC sender operations
#[async_trait]
pub trait ViaBtcSenderDalOps {
    /// List new inscription requests that haven't been processed yet
    async fn list_new_inscription_requests(
        &mut self,
        limit: i64,
    ) -> anyhow::Result<Vec<CommonInscriptionRequest>>;

    /// Get all inflight inscriptions (sent but not confirmed)
    async fn get_inflight_inscriptions(&mut self) -> anyhow::Result<Vec<CommonInscriptionRequest>>;

    /// Get the last history entry for a given inscription request
    async fn get_last_inscription_history(
        &mut self,
        inscription_id: i64,
    ) -> anyhow::Result<Option<CommonInscriptionHistory>>;

    /// Insert a new inscription request history
    async fn insert_inscription_history(
        &mut self,
        inscription_id: i64,
        input: InscriptionHistoryInput<'_>,
    ) -> anyhow::Result<i64>;

    /// Confirm an inscription (mark it as confirmed)
    async fn confirm_inscription(
        &mut self,
        inscription_id: i64,
        history_id: i64,
    ) -> anyhow::Result<()>;
}
