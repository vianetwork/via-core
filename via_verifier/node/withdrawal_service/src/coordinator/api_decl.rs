use std::sync::Arc;

use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use via_btc_client::withdrawal_builder::WithdrawalBuilder;
use via_verifier_dal::{ConnectionPool, Core};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::configs::via_verifier::ViaVerifierConfig;

use crate::types::{SigningSession, ViaWithdrawalState};

pub struct RestApi {
    pub master_connection_pool: ConnectionPool<Core>,
    pub state: ViaWithdrawalState,
    pub withdrawal_builder: WithdrawalBuilder,
    pub withdrawal_client: WithdrawalClient,
}

impl RestApi {
    pub fn new(
        config: ViaVerifierConfig,
        master_connection_pool: ConnectionPool<Core>,
        withdrawal_builder: WithdrawalBuilder,
        withdrawal_client: WithdrawalClient,
    ) -> anyhow::Result<Self> {
        let state = ViaWithdrawalState {
            signing_session: Arc::new(RwLock::new(SigningSession::default())),
            required_signers: config.required_signers,
        };
        Ok(Self {
            master_connection_pool,
            state,
            withdrawal_builder,
            withdrawal_client,
        })
    }

    pub fn into_router(self) -> axum::Router<()> {
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
            .layer(CorsLayer::permissive());

        axum::Router::new()
            .nest("/session", router)
            .with_state(Arc::new(self))
    }
}
