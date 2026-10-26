use anyhow::Context;
use bitcoin::Txid;
use via_btc_client::{inscriber::Inscriber, traits::Serializable, types::InscriptionMessage};
use zksync_db_connection::{connection_pool::ConnectionPool, instrument::InstrumentExt};
use zksync_types::via_btc_sender::ViaBtcInscriptionRequest;

use crate::ReaderRole;

pub struct Publication {
    pub commit_txid: Txid,
    pub reveal_txid: Txid,
    pub signed_commit: Vec<u8>,
    pub signed_reveal: Vec<u8>,
    pub fees: i64,
    pub sent_at: i64,
}

pub async fn publish<DB: ReaderRole>(
    pool: &ConnectionPool<DB>,
    inscriber: &mut Inscriber,
    request: &ViaBtcInscriptionRequest,
) -> anyhow::Result<bool> {
    let mut authority =
        crate::transition::begin_authority(pool, inscriber.get_client().await, &[]).await?;
    let pending = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM via_btc_inscriptions_request r
        WHERE r.id=$1 AND r.inscription_message=$2 AND NOT EXISTS(SELECT 1 FROM via_btc_inscriptions_request_history h WHERE h.inscription_request_id=r.id))")
        .bind(request.id).bind(&request.inscription_message).instrument("bitcoin_publication_pending").fetch_one(&mut authority).await?;
    if !pending {
        return Ok(false);
    }
    let input =
        InscriptionMessage::from_bytes(request.inscription_message.as_deref().unwrap_or_default());
    DB::authorize_publication(&mut authority, &input, &inscriber.inscriber_address()?).await?;
    let sent_at = i64::try_from(inscriber.get_client().await.fetch_block_height().await?)?;
    let started = std::time::Instant::now();
    let info = match inscriber.inscribe(input).await {
        Ok(info) => info,
        Err(error) => {
            crate::metrics::METRICS.inscription_errors[&DB::NAME].inc();
            return Err(error);
        }
    };
    crate::metrics::METRICS.inscription_time[&DB::NAME].observe(started.elapsed());

    let publication = Publication {
        commit_txid: info.final_commit_tx.txid,
        reveal_txid: info.final_reveal_tx.txid,
        signed_commit: bincode::serialize(&info.final_commit_tx.tx)
            .context("Error serializing the commit tx")?,
        signed_reveal: bincode::serialize(&info.final_reveal_tx.tx)
            .context("Error serializing the reveal tx")?,
        fees: i64::try_from(
            (info.reveal_tx_output_info._reveal_fee + info.commit_tx_output_info.commit_tx_fee)
                .to_sat(),
        )?,
        sent_at,
    };
    DB::save_publication(&mut authority, request.id, publication).await?;
    authority.commit().await?;
    Ok(true)
}

#[derive(sqlx::FromRow)]
struct PendingPublication {
    id: i64,
    request_type: String,
    inscription_message: Option<Vec<u8>>,
    predicted_fee: Option<i64>,
    confirmed_inscriptions_request_history_id: Option<i64>,
    created_at: sqlx::types::chrono::NaiveDateTime,
    updated_at: sqlx::types::chrono::NaiveDateTime,
}

pub async fn publish_pending<DB: ReaderRole>(
    pool: &ConnectionPool<DB>,
    inscriber: &mut Inscriber,
    max_in_flight: i64,
) -> anyhow::Result<()> {
    let mut storage = pool.connection().await?;
    let in_flight = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM via_btc_inscriptions_request r
        WHERE confirmed_inscriptions_request_history_id IS NULL AND EXISTS(SELECT 1 FROM via_btc_inscriptions_request_history h WHERE h.inscription_request_id=r.id)")
        .instrument("bitcoin_inflight_publications").fetch_one(&mut storage).await?;
    crate::metrics::METRICS.in_flight[&DB::NAME].set(usize::try_from(in_flight)?);
    let pending = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM via_btc_inscriptions_request r
        WHERE NOT EXISTS(SELECT 1 FROM via_btc_inscriptions_request_history h WHERE h.inscription_request_id=r.id)")
        .instrument("bitcoin_pending_publication_count").fetch_one(&mut storage).await?;
    crate::metrics::METRICS.pending[&DB::NAME].set(usize::try_from(pending)?);
    let balance = inscriber.get_balance().await?;
    crate::metrics::METRICS.balance[&(DB::NAME, inscriber.inscriber_address()?.to_string())]
        .set(usize::try_from(balance)?);
    let requests = sqlx::query_as::<_, PendingPublication>("SELECT r.id,r.request_type,r.inscription_message,r.predicted_fee,
        r.confirmed_inscriptions_request_history_id,r.created_at,r.updated_at FROM via_btc_inscriptions_request r
        WHERE NOT EXISTS(SELECT 1 FROM via_btc_inscriptions_request_history h WHERE h.inscription_request_id=r.id)
        ORDER BY r.id LIMIT $1")
        .bind(max_in_flight.saturating_sub(in_flight).max(0)).instrument("bitcoin_pending_publications").fetch_all(&mut storage).await?;
    drop(storage);
    for request in requests {
        let request = ViaBtcInscriptionRequest {
            id: request.id,
            request_type: request.request_type,
            inscription_message: request.inscription_message,
            predicted_fee: request.predicted_fee,
            confirmed_inscriptions_request_history_id: request
                .confirmed_inscriptions_request_history_id,
            created_at: request.created_at,
            updated_at: request.updated_at,
        };
        if publish(pool, inscriber, &request).await? {
            crate::metrics::METRICS.pending[&DB::NAME].dec_by(1);
            crate::metrics::METRICS.in_flight[&DB::NAME].inc_by(1);
        }
    }
    Ok(())
}

pub async fn stuck_inscription_height(
    client: &dyn via_btc_client::traits::BitcoinOps,
    sent_at: i64,
    after_blocks: u32,
) -> anyhow::Result<Option<u64>> {
    let current_block = client.fetch_block_height().await?;
    Ok((sent_at + i64::from(after_blocks) <= current_block as i64).then_some(current_block))
}
