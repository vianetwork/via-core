use std::{str::FromStr, sync::Arc};

use axum::middleware;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use via_btc_client::withdrawal_builder::WithdrawalBuilder;
use via_verifier_dal::{ConnectionPool, Verifier};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::configs::via_verifier::ViaVerifierConfig;

use crate::{
    coordinator::auth_middleware,
    types::{SigningSession, ViaWithdrawalState},
};

pub struct RestApi {
    pub master_connection_pool: ConnectionPool<Verifier>,
    pub state: ViaWithdrawalState,
    pub withdrawal_builder: WithdrawalBuilder,
    pub withdrawal_client: WithdrawalClient,
}

impl RestApi {
    pub fn new(
        config: ViaVerifierConfig,
        master_connection_pool: ConnectionPool<Verifier>,
        withdrawal_builder: WithdrawalBuilder,
        withdrawal_client: WithdrawalClient,
    ) -> anyhow::Result<Self> {
        let state = ViaWithdrawalState {
            signing_session: Arc::new(RwLock::new(SigningSession::default())),
            required_signers: config.required_signers,
            verifiers_pub_keys: config
                .verifiers_pub_keys_str
                .iter()
                .map(|s| bitcoin::secp256k1::PublicKey::from_str(s).unwrap())
                .collect(),
        };
        Ok(Self {
            master_connection_pool,
            state,
            withdrawal_builder,
            withdrawal_client,
        })
    }

    pub fn into_router(self) -> axum::Router<()> {
        // Wrap the API state in an Arc.
        let shared_state = Arc::new(self);

        // Create middleware layers using from_fn_with_state.
        let auth_mw =
            middleware::from_fn_with_state(shared_state.clone(), auth_middleware::auth_middleware);
        let body_mw =
            middleware::from_fn_with_state(shared_state.clone(), auth_middleware::extract_body);

        let router = axum::Router::new()
            .route("/new", axum::routing::post(Self::new_session))
            .route("/", axum::routing::get(Self::get_session))
            .route(
                "/signature",
                axum::routing::post(Self::submit_partial_signature),
            )
            .route(
                "/signature",
                axum::routing::get(Self::get_submitted_signatures),
            )
            .route("/nonce", axum::routing::post(Self::submit_nonce))
            .route("/nonce", axum::routing::get(Self::get_nonces))
            .route_layer(body_mw)
            .route_layer(auth_mw)
            .with_state(shared_state.clone())
            .layer(CorsLayer::permissive());

        axum::Router::new().nest("/session", router)
    }
}
