use bitcoin::Txid;
use chrono::NaiveDateTime;

#[derive(Clone)]
pub struct ViaBtcInscriptionRequest {
    pub id: i64,
    pub request_type: String,
    pub inscription_message: Option<Vec<u8>>,
    pub predicted_fee: Option<i64>,
    pub confirmed_inscriptions_request_history_id: Option<i64>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl std::fmt::Debug for ViaBtcInscriptionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BtcInscriptionRequest")
            .field("id", &self.id)
            .field("request_type", &self.request_type)
            .field("inscription_message", &self.inscription_message)
            .field(
                "confirmed_inscriptions_request_history_id",
                &self.confirmed_inscriptions_request_history_id,
            )
            .field("predicted_fee", &self.predicted_fee)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct ViaBtcInscriptionRequestHistory {
    pub id: i64,
    pub commit_tx_id: Txid,
    pub reveal_tx_id: Txid,
    pub inscription_request_id: i64,
    pub signed_commit_tx: Option<Vec<u8>>,
    pub signed_reveal_tx: Option<Vec<u8>>,
    pub actual_fees: i64,
    pub sent_at_block: i64,
    pub confirmed_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}
