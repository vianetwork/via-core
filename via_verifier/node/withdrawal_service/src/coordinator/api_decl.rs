use std::{str::FromStr, sync::Arc};

use bitcoin::{Address, Network};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use via_btc_client::withdrawal_builder::WithdrawalBuilder;
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::configs::via_verifier::ViaVerifierConfig;

use crate::{
    types::{SigningSession, ViaWithdrawalState},
    utils::get_signer,
};

pub struct RestApi {
    pub state: ViaWithdrawalState,
    pub withdrawal_builder: WithdrawalBuilder,
    pub withdrawal_client: WithdrawalClient,
}

impl RestApi {
    pub fn new(
        network: Network,
        config: ViaVerifierConfig,
        withdrawal_builder: WithdrawalBuilder,
        withdrawal_client: WithdrawalClient,
    ) -> anyhow::Result<Self> {
        let signer = get_signer(&config.private_key, config.verifiers_pub_keys_str)?;

        let bridge_address =
            Address::from_str(config.bridge_address_str.as_str())?.require_network(network)?;

        let state = ViaWithdrawalState {
            signer: Arc::new(RwLock::new(signer)),
            signing_session: Arc::new(RwLock::new(SigningSession::default())),
            unsigned_tx: Arc::new(RwLock::new(None)),
            bridge_address,
            required_signers: config.required_signers,
        };
        Ok(Self {
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
            .route("/signature", axum::routing::get(Self::get_final_signature))
            .route("/nonce", axum::routing::post(Self::submit_nonce))
            .route("/nonce", axum::routing::get(Self::get_nonces))
            .layer(CorsLayer::permissive());

        axum::Router::new()
            .nest("/session", router)
            .with_state(Arc::new(self))
    }
}
