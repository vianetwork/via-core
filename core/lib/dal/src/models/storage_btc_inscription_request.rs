use std::str::FromStr;

use bitcoin::Txid;
use sqlx::types::chrono::NaiveDateTime;
use zksync_types::{
    btc_inscription_operations::ViaBtcInscriptionRequestType,
    btc_sender::{ViaBtcInscriptionRequest, ViaBtcInscriptionRequestHistory},
};

#[derive(Debug, Clone)]
pub struct ViaStorageBtcInscriptionRequest {
    pub id: i64,
    pub request_type: String,
    pub inscription_message: Option<Vec<u8>>,
    pub predicted_fee: Option<i64>,
    pub confirmed_inscriptions_request_history_id: Option<i64>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Clone, Debug)]
pub struct ViaStorageBtcInscriptionRequestHistory {
    pub id: i64,
    pub commit_tx_id: String,
    pub reveal_tx_id: String,
    pub inscription_request_id: i64,
    pub signed_commit_tx: Option<Vec<u8>>,
    pub signed_reveal_tx: Option<Vec<u8>>,
    pub actual_fees: i64,
    pub sent_at_block: i64,
    pub confirmed_at: Option<NaiveDateTime>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

impl From<ViaStorageBtcInscriptionRequest> for ViaBtcInscriptionRequest {
    fn from(req: ViaStorageBtcInscriptionRequest) -> ViaBtcInscriptionRequest {
        ViaBtcInscriptionRequest {
            id: req.id,
            request_type: ViaBtcInscriptionRequestType::from_str(&req.request_type).unwrap(),
            inscription_message: req.inscription_message,
            confirmed_inscriptions_request_history_id: req
                .confirmed_inscriptions_request_history_id,
            predicted_fee: req.predicted_fee,
            created_at: req.created_at,
            updated_at: req.updated_at,
        }
    }
}

impl From<ViaStorageBtcInscriptionRequestHistory> for ViaBtcInscriptionRequestHistory {
    fn from(history: ViaStorageBtcInscriptionRequestHistory) -> ViaBtcInscriptionRequestHistory {
        ViaBtcInscriptionRequestHistory {
            id: history.id,
            commit_tx_id: Txid::from_str(&history.commit_tx_id).unwrap(),
            reveal_tx_id: Txid::from_str(&history.reveal_tx_id).unwrap(),
            inscription_request_id: history.inscription_request_id,
            sent_at_block: history.sent_at_block,
            signed_commit_tx: history.signed_commit_tx,
            signed_reveal_tx: history.signed_reveal_tx,
            actual_fees: history.actual_fees,
            confirmed_at: history.confirmed_at,
        }
    }
}
