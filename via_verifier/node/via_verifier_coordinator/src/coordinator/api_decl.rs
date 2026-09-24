use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};

use axum::middleware;
use tokio::sync::RwLock;
use tower::ServiceBuilder;
use tower_http::{cors::CorsLayer, timeout::TimeoutLayer};
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
    pub config: ViaVerifierConfig,
    pub state: ViaWithdrawalState,
    pub session_manager: SessionManager,
    pub master_connection_pool: ConnectionPool<Verifier>,
    pub response_key: bitcoin::secp256k1::SecretKey,
    pub challenges: tokio::sync::Mutex<HashMap<(usize, [u8; 32]), i64>>,
    pub creation: tokio::sync::Mutex<()>,
    pub network: bitcoin::Network,
}

const API_TIMEOUT: Duration = Duration::from_secs(30);

impl RestApi {
    pub fn new(
        config: ViaVerifierConfig,
        master_connection_pool: ConnectionPool<Verifier>,
        btc_client: Arc<dyn BitcoinOps>,
        withdrawal_client: WithdrawalClient,
        verifiers_pub_keys: Vec<String>,
        coordinator_private_key: String,
    ) -> anyhow::Result<Self> {
        let response_key = bitcoin::PrivateKey::from_wif(&coordinator_private_key)?.inner;
        let public = bitcoin::secp256k1::PublicKey::from_secret_key(
            &bitcoin::secp256k1::Secp256k1::new(),
            &response_key,
        )
        .to_string();
        anyhow::ensure!(
            public == config.coordinator_public_key && verifiers_pub_keys.contains(&public),
            "Coordinator authentication key is not the configured participant"
        );
        let state = ViaWithdrawalState {
            signing_session: Arc::new(RwLock::new(SigningSession::default())),
            verifiers_pub_keys: verifiers_pub_keys
                .iter()
                .map(|s| bitcoin::secp256k1::PublicKey::from_str(s))
                .collect::<Result<_, _>>()?,
        };

        let transaction_builder = Arc::new(TransactionBuilder::new(btc_client.clone())?);

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
            config,
            session_manager: SessionManager::new(sessions),
            state,
            master_connection_pool,
            response_key,
            challenges: tokio::sync::Mutex::new(HashMap::new()),
            creation: tokio::sync::Mutex::new(()),
            network: btc_client.get_network(),
        })
    }

    pub fn into_router(self) -> axum::Router<()> {
        // Wrap the API state in an Arc.
        let shared_state = Arc::new(self);

        // Create middleware layers using from_fn_with_state.
        let auth_mw =
            middleware::from_fn_with_state(shared_state.clone(), auth_middleware::auth_middleware);

        axum::Router::new()
            .route("/session/new", axum::routing::post(Self::new_session))
            .route("/session/", axum::routing::get(Self::get_session))
            .route(
                "/session/signature",
                axum::routing::post(Self::submit_partial_signature),
            )
            .route("/session/nonce", axum::routing::post(Self::submit_nonce))
            .route_layer(auth_mw)
            .with_state(shared_state.clone())
            .layer(
                ServiceBuilder::new()
                    .layer(TimeoutLayer::new(API_TIMEOUT))
                    .layer(CorsLayer::permissive())
                    .into_inner(),
            )
    }
}
