//! Original coordinator handlers, middleware, withdrawal producer and DTO from the
//! fixed pre-change commit. Keep the old JSON/auth path executable over real TCP.

use std::{ops::Deref, sync::Arc};

use via_btc_client::traits::BitcoinOps;
use via_musig2::transaction_builder::TransactionBuilder;

use crate::{coordinator::api_decl::RestApi as CurrentRestApi, types::SessionType};

mod api_impl;
mod auth;
mod auth_middleware;
mod error;
mod snapshot;
mod withdrawal;
pub(super) use snapshot::SigningSessionResponse;

struct RestApi(CurrentRestApi);

impl Deref for RestApi {
    type Target = CurrentRestApi;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub(super) fn router(mut api: CurrentRestApi, btc: Arc<dyn BitcoinOps>) -> axum::Router {
    api.session_manager.sessions.insert(
        SessionType::Withdrawal,
        Arc::new(withdrawal::WithdrawalSession::new(
            api.master_connection_pool.clone(),
            Arc::new(TransactionBuilder::new(btc).unwrap()),
            super::tests::withdrawal_client(),
        )),
    );
    let state = Arc::new(RestApi(api));
    let auth =
        axum::middleware::from_fn_with_state(state.clone(), auth_middleware::auth_middleware);
    axum::Router::new()
        .route("/session/", axum::routing::get(RestApi::get_session))
        .route("/session/new", axum::routing::post(RestApi::new_session))
        .route("/session/nonce", axum::routing::post(RestApi::submit_nonce))
        .route("/session/signature", axum::routing::post(RestApi::submit_partial_signature))
        .route_layer(auth)
        .with_state(state)
}

pub(super) fn reply_body(
    request: &crate::auth::Binding,
    bytes: &[u8],
    key: &bitcoin::secp256k1::PublicKey,
    status: u16,
) -> anyhow::Result<Vec<u8>> {
    let request: auth::Binding = serde_json::from_slice(&serde_json::to_vec(request)?)?;
    let envelope: auth::Envelope = serde_json::from_slice(bytes)?;
    envelope.verify_response(&request, key, status, chrono::Utc::now().timestamp(), 30)?;
    Ok(envelope.body)
}
