use std::{collections::HashMap, str::FromStr, sync::Arc};

use axum::middleware;
use bitcoin::Address;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use via_btc_client::traits::BitcoinOps;
use via_musig2::transaction_builder::TransactionBuilder;
use via_verifier_dal::{ConnectionPool, Verifier};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::configs::via_verifier::ViaVerifierConfig;

use crate::{
    coordinator::auth_middleware,
    sessions::{session_manager::SessionManager, withdrawal::WithdrawalSession},
    traits::ISession,
    types::{SessionType, SigningSession, ViaWithdrawalState},
};

pub struct RestApi {
    pub state: ViaWithdrawalState,
    pub session_manager: SessionManager,
    pub master_connection_pool: ConnectionPool<Verifier>,
}

impl RestApi {
    pub fn new(
        config: ViaVerifierConfig,
        master_connection_pool: ConnectionPool<Verifier>,
        btc_client: Arc<dyn BitcoinOps>,
        withdrawal_client: WithdrawalClient,
        bridge_address: Address,
        verifiers_pub_keys: Vec<String>,
        required_signers: usize,
    ) -> anyhow::Result<Self> {
        let state = ViaWithdrawalState {
            signing_session: Arc::new(RwLock::new(SigningSession::default())),
            required_signers,
            verifiers_pub_keys: verifiers_pub_keys
                .iter()
                .map(|s| bitcoin::secp256k1::PublicKey::from_str(s).unwrap())
                .collect(),
            verifier_request_timeout: config.verifier_request_timeout,
        };

        let transaction_builder =
            Arc::new(TransactionBuilder::new(btc_client.clone(), bridge_address)?);

        let withdrawal_session = WithdrawalSession::new(
            master_connection_pool.clone(),
            transaction_builder.clone(),
            withdrawal_client.clone(),
        );

        // Add sessions type the verifier network can process
        let sessions: HashMap<SessionType, Arc<dyn ISession>> = [(
            SessionType::Withdrawal,
            Arc::new(withdrawal_session) as Arc<dyn ISession>,
        )]
        .into_iter()
        .collect();

        Ok(Self {
            session_manager: SessionManager::new(sessions),
            state,
            master_connection_pool,
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
