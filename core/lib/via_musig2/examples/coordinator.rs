use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use bitcoin::{Address, Amount, Network, Txid};
use musig2::{CompactSignature, PartialSignature, PubNonce};
use secp256k1_musig2::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use via_btc_client::{
    types::BitcoinNetwork,
    withdrawal::{UnsignedWithdrawalTx, WithdrawalBuilder, WithdrawalRequest},
};

// Shared state for managing signing sessions
struct AppState {
    signer: via_musig2::Signer,
    signing_sessions: HashMap<String, SigningSession>,
    unsigned_txs: HashMap<String, UnsignedWithdrawalTx>,
    bridge_address: Address,
}

#[derive(Debug, Clone)]
struct SigningSession {
    session_id: String,
    tx_id: String,
    received_nonces: HashMap<usize, PubNonce>,
    received_sigs: HashMap<usize, PartialSignature>,
    final_signature: Option<CompactSignature>,
}

#[derive(Serialize, Deserialize)]
struct NoncePair {
    signer_index: usize,
    nonce: String, // Base64 encoded
}

#[derive(Serialize, Deserialize)]
struct PartialSignaturePair {
    signer_index: usize,
    signature: String, // Base64 encoded
}

#[derive(Serialize, Deserialize)]
struct SigningSessionResponse {
    session_id: String,
    message_to_sign: String,
    aggregated_pubkey: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize coordinator's signer (also acts as a verifier)
    let mut rng = rand::thread_rng();
    let secret_key = SecretKey::new(&mut rng);

    // For demo, we'll use 3 verifiers (including coordinator)
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let public_key = PublicKey::from_secret_key(&secp, &secret_key);
    let all_pubkeys = vec![
        public_key.clone(),
        // Add other verifiers' public keys
        // These would normally come from configuration
        PublicKey::from_secret_key(&secp, &SecretKey::new(&mut rng)),
        PublicKey::from_secret_key(&secp, &SecretKey::new(&mut rng)),
    ];

    let signer = via_musig2::Signer::new(secret_key, 0, all_pubkeys)?;

    // Create test bridge address
    let bridge_address =
        Address::from_str("bcrt1pxqkh0g270lucjafgngmwv7vtgc8mk9j5y4j8fnrxm77yunuh398qfv8tqp")?
            .require_network(Network::Regtest)?;

    // Initialize shared state
    let state = Arc::new(RwLock::new(AppState {
        signer,
        signing_sessions: HashMap::new(),
        unsigned_txs: HashMap::new(),
        bridge_address,
    }));

    // Build router
    let app = Router::new()
        .route("/session/:id", get(get_session))
        .route("/session/:id/nonce", post(submit_nonce))
        .route("/session/:id/partial", post(submit_partial_signature))
        .route("/session/:id/signature", get(get_final_signature))
        .with_state(state);

    // Start server
    println!("Starting coordinator server on 0.0.0.0:3000");
    axum::Server::bind(&"0.0.0.0:3000".parse()?)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

// Create new signing session for a withdrawal transaction
async fn create_signing_session(
    state: &RwLock<AppState>,
) -> anyhow::Result<SigningSessionResponse> {
    let mut state = state.write().await;

    // Create test withdrawal transaction
    let withdrawal_builder = create_test_withdrawal_builder().await?;
    let withdrawals = create_test_withdrawal_requests()?;
    let proof_txid = Txid::from_slice(&[0x42; 32])?;

    let unsigned_tx = withdrawal_builder
        .create_unsigned_withdrawal_tx(withdrawals, proof_txid)
        .await?;

    // Create unique session ID
    let session_id = uuid::Uuid::new_v4().to_string();
    let tx_id = unsigned_tx.txid.to_string();

    // Store unsigned transaction
    state
        .unsigned_txs
        .insert(tx_id.clone(), unsigned_tx.clone());

    // Initialize signing session
    let session = SigningSession {
        session_id: session_id.clone(),
        tx_id,
        received_nonces: HashMap::new(),
        received_sigs: HashMap::new(),
        final_signature: None,
    };

    state.signing_sessions.insert(session_id.clone(), session);

    // Start signing session with message (transaction hash)
    let message = unsigned_tx.tx.compute_txid().as_raw_hash().to_vec();
    let nonce = state.signer.start_signing_session(message.clone())?;

    // Store coordinator's own nonce
    if let Some(session) = state.signing_sessions.get_mut(&session_id) {
        session.received_nonces.insert(0, nonce);
    }

    Ok(SigningSessionResponse {
        session_id,
        message_to_sign: hex::encode(message),
        aggregated_pubkey: hex::encode(state.signer.aggregated_pubkey().serialize()),
    })
}

// Handler implementations
async fn get_session(
    State(state): State<Arc<RwLock<AppState>>>,
    Path(session_id): Path<String>,
) -> Result<Json<SigningSessionResponse>, StatusCode> {
    // Implementation
    todo!()
}

async fn submit_nonce(
    State(state): State<Arc<RwLock<AppState>>>,
    Path(session_id): Path<String>,
    Json(nonce_pair): Json<NoncePair>,
) -> Result<StatusCode, StatusCode> {
    // Implementation
    todo!()
}

async fn submit_partial_signature(
    State(state): State<Arc<RwLock<AppState>>>,
    Path(session_id): Path<String>,
    Json(sig_pair): Json<PartialSignaturePair>,
) -> Result<StatusCode, StatusCode> {
    // Implementation
    todo!()
}

async fn get_final_signature(
    State(state): State<Arc<RwLock<AppState>>>,
    Path(session_id): Path<String>,
) -> Result<Json<String>, StatusCode> {
    // Implementation
    todo!()
}

// Helper functions
async fn create_test_withdrawal_builder() -> anyhow::Result<WithdrawalBuilder> {
    // Similar to withdrawal_builder test setup
    todo!()
}

fn create_test_withdrawal_requests() -> anyhow::Result<Vec<WithdrawalRequest>> {
    // Similar to withdrawal_builder test setup
    todo!()
}
