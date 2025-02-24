use std::{clone::Clone, collections::HashMap, str::FromStr, sync::Arc, time::Duration};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use bitcoin::{hashes::Hash, Address, Amount, Network, TxOut, Txid};
use hyper::Server;
use musig2::{BinaryEncoding, CompactSignature, PartialSignature, PubNonce};
use rand::thread_rng;
use secp256k1_musig2::{PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, instrument};
use uuid::Uuid;
use via_btc_client::{client::BitcoinClient, types::BitcoinNetwork};
use via_musig2::{transaction_builder::TransactionBuilder, verify_signature, Signer};
use via_verifier_types::{transaction::UnsignedBridgeTx, withdrawal::WithdrawalRequest};

#[derive(Clone)]
#[allow(dead_code)]
struct AppState {
    signer: Arc<RwLock<Signer>>,
    signing_sessions: Arc<RwLock<HashMap<String, SigningSession>>>,
    unsigned_txs: Arc<RwLock<HashMap<String, UnsignedBridgeTx>>>,
    bridge_address: Address,
    all_pubkeys: Vec<PublicKey>,
    num_signers: usize,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SigningSession {
    session_id: String,
    tx_id: String,
    received_nonces: HashMap<usize, PubNonce>,
    received_sigs: HashMap<usize, PartialSignature>,
    final_signature: Option<CompactSignature>,
    message: Vec<u8>,
}

/// Data posted by other signers to submit their nonce
#[derive(Serialize, Deserialize, Debug)]
struct NoncePair {
    signer_index: usize,
    nonce: String, // Base64 encoded
}

/// Data posted by other signers to submit their partial signature
#[derive(Serialize, Deserialize, Debug)]
struct PartialSignaturePair {
    signer_index: usize,
    signature: String, // Base64 encoded
}

#[derive(Serialize, Deserialize, Clone)]
struct SigningSessionResponse {
    session_id: String,
    message_to_sign: String,   // hex-encoded message (txid)
    aggregated_pubkey: String, // hex-encoded aggregated pubkey
    required_signers: usize,
    received_nonces: usize,
    received_partial_signatures: usize,
    final_signature: Option<String>, // hex-encoded final signature if present
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Setup coordinator keys and signers
    let mut rng = thread_rng();
    let secret_key = SecretKey::new(&mut rng);

    let secp = Secp256k1::new();
    let public_key = PublicKey::from_secret_key(&secp, &secret_key);
    let other_pubkey_1 = PublicKey::from_secret_key(&secp, &SecretKey::new(&mut rng));
    let other_pubkey_2 = PublicKey::from_secret_key(&secp, &SecretKey::new(&mut rng));

    let all_pubkeys = vec![public_key, other_pubkey_1, other_pubkey_2];
    let coordinator_signer = Signer::new(secret_key, 0, all_pubkeys.clone())?;

    // Create test bridge address
    let bridge_address =
        Address::from_str("bcrt1pxqkh0g270lucjafgngmwv7vtgc8mk9j5y4j8fnrxm77yunuh398qfv8tqp")?
            .require_network(Network::Regtest)?;

    let state = AppState {
        signer: Arc::new(RwLock::new(coordinator_signer)),
        signing_sessions: Arc::new(RwLock::new(HashMap::new())),
        unsigned_txs: Arc::new(RwLock::new(HashMap::new())),
        bridge_address,
        all_pubkeys: all_pubkeys.clone(),
        num_signers: 3,
    };

    // Start coordinator server in one task
    let server_state = state.clone();
    let server_task = tokio::spawn(async move {
        run_coordinator_server(server_state).await.unwrap();
    });

    // Wait a bit for the server to start
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Create one signing session for demonstration
    let session_id = create_signing_session(&state).await?;

    // Now simulate other verifiers (signer_index = 1 and 2)
    // Each verifier has their own keys:
    let mut rng = thread_rng();
    let verifier1_sk = SecretKey::new(&mut rng);
    let verifier1_signer = Signer::new(verifier1_sk, 1, all_pubkeys.clone())?;

    let mut rng = thread_rng();
    let verifier2_sk = SecretKey::new(&mut rng);
    let verifier2_signer = Signer::new(verifier2_sk, 2, all_pubkeys)?;

    // Spawn tasks for verifier polling
    let verifier1_task = tokio::spawn(run_verifier_polling(
        "http://0.0.0.0:3000".to_string(),
        session_id.session_id.clone(),
        verifier1_signer,
    ));

    let verifier2_task = tokio::spawn(run_verifier_polling(
        "http://0.0.0.0:3000".to_string(),
        session_id.session_id.clone(),
        verifier2_signer,
    ));

    // Run all concurrently
    let _ = tokio::join!(server_task, verifier1_task, verifier2_task);

    Ok(())
}

async fn run_coordinator_server(state: AppState) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/session/new", post(create_session_handler))
        .route("/session/:id", get(get_session))
        .route("/session/:id/nonce", post(submit_nonce))
        .route("/session/:id/partial", post(submit_partial_signature))
        .route("/session/:id/signature", get(get_final_signature))
        .route("/session/:id/nonces", get(get_nonces))
        .with_state(state);

    info!("Starting coordinator server on 0.0.0.0:3000");
    Server::bind(&"0.0.0.0:3000".parse()?)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

async fn run_verifier_polling(
    base_url: String,
    session_id: String,
    mut signer: Signer,
) -> anyhow::Result<()> {
    use reqwest::Client;

    let client = Client::new();

    loop {
        // Fetch session info
        let url = format!("{}/session/{}", base_url, session_id);
        let resp = client.get(&url).send().await?;
        if resp.status().as_u16() == StatusCode::NOT_FOUND.as_u16() {
            // Session might not exist yet, wait and retry
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if !resp.status().is_success() {
            println!(
                "Verifier polling: Error fetching session info: {:?}",
                resp.text().await?
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        let session_info: SigningSessionResponse = resp.json().await?;
        if session_info.final_signature.is_some() {
            println!(
                "Verifier {}: Final signature obtained! {:?}",
                signer.signer_index(),
                session_info.final_signature
            );
            break;
        }

        // We need to see if we have submitted our nonce and partial signature
        // If we have not submitted nonce and partial sig yet, we do so if needed:
        if session_info.received_nonces < session_info.required_signers {
            // We need to submit nonce if not already submitted
            // Start signing session if not started:
            let message = hex::decode(&session_info.message_to_sign)?;
            if signer.has_not_started() {
                signer.start_signing_session(message)?;
            }

            if !signer.has_submitted_nonce() {
                // Submit our nonce
                let nonce = signer
                    .our_nonce()
                    .ok_or_else(|| anyhow::anyhow!("No nonce available"))?;
                let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(nonce.to_bytes());
                let nonce_pair = NoncePair {
                    signer_index: signer.signer_index(),
                    nonce: nonce_b64,
                };
                let nonce_url = format!("{}/session/{}/nonce", base_url, session_id);
                let resp = client.post(&nonce_url).json(&nonce_pair).send().await?;
                if !resp.status().is_success() {
                    println!(
                        "Verifier {}: Error submitting nonce: {:?}",
                        signer.signer_index(),
                        resp.text().await?
                    );
                } else {
                    signer.mark_nonce_submitted();
                }
            }
        } else if session_info.received_partial_signatures < session_info.required_signers {
            // All nonces are in, we can finalize first round and create partial signature if not done
            if !signer.has_created_partial_sig() {
                // We need to fetch all nonces from the coordinator
                let nonces_url = format!("{}/session/{}/nonces", base_url, session_id);
                let resp = client.get(&nonces_url).send().await?;
                let nonces: HashMap<usize, String> = resp.json().await?;

                // Process each nonce
                for (idx, nonce_b64) in nonces {
                    if idx != signer.signer_index() {
                        let nonce_bytes =
                            base64::engine::general_purpose::STANDARD.decode(nonce_b64)?;
                        let nonce = PubNonce::from_bytes(&nonce_bytes)?;
                        signer
                            .receive_nonce(idx, nonce.clone())
                            .map_err(|e| anyhow::anyhow!("Failed to receive nonce: {}", e))?;
                    }
                }

                let partial_sig = signer.create_partial_signature()?;
                let sig_b64 =
                    base64::engine::general_purpose::STANDARD.encode(partial_sig.serialize());
                let sig_pair = PartialSignaturePair {
                    signer_index: signer.signer_index(),
                    signature: sig_b64,
                };
                let partial_url = format!("{}/session/{}/partial", base_url, session_id);
                let resp = client.post(&partial_url).json(&sig_pair).send().await?;
                if !resp.status().is_success() {
                    println!(
                        "Verifier {}: Error submitting partial signature: {:?}",
                        signer.signer_index(),
                        resp.text().await?
                    );
                } else {
                    signer.mark_partial_sig_submitted();
                }
            }
        } else {
            // waiting for final signature
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    Ok(())
}

// Handler to create a new signing session for a withdrawal transaction
#[instrument(skip(state))]
async fn create_session_handler(
    State(state): State<AppState>,
) -> Result<Json<SigningSessionResponse>, StatusCode> {
    create_signing_session(&state)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

// GET /session/:id
#[instrument(skip(state))]
async fn get_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SigningSessionResponse>, StatusCode> {
    let sessions = state.signing_sessions.read().await;
    let session = sessions.get(&session_id).ok_or(StatusCode::NOT_FOUND)?;

    let signer = state.signer.read().await;
    let resp = SigningSessionResponse {
        session_id: session.session_id.clone(),
        message_to_sign: hex::encode(&session.message),
        aggregated_pubkey: hex::encode(signer.aggregated_pubkey().serialize()),
        required_signers: state.num_signers,
        received_nonces: session.received_nonces.len(),
        received_partial_signatures: session.received_sigs.len(),
        final_signature: session
            .final_signature
            .as_ref()
            .map(|sig| hex::encode(sig.serialize())),
    };
    Ok(Json(resp))
}

// POST /session/:id/nonce
#[instrument(skip(state))]
async fn submit_nonce(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(nonce_pair): Json<NoncePair>,
) -> Result<StatusCode, StatusCode> {
    let decoded_nonce = base64::engine::general_purpose::STANDARD
        .decode(&nonce_pair.nonce)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let pub_nonce = PubNonce::from_bytes(&decoded_nonce).map_err(|_| StatusCode::BAD_REQUEST)?;

    {
        let mut sessions = state.signing_sessions.write().await;
        let session = sessions.get_mut(&session_id).ok_or(StatusCode::NOT_FOUND)?;

        session
            .received_nonces
            .insert(nonce_pair.signer_index, pub_nonce);

        // If all nonces are collected, coordinator finalizes and create partial sig
        if session.received_nonces.len() == state.num_signers {
            let mut signer = state.signer.write().await;
            for (&i, nonce) in &session.received_nonces {
                if i != 0 {
                    signer
                        .receive_nonce(i, nonce.clone())
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                }
            }
            let partial_sig = signer
                .create_partial_signature()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            session.received_sigs.insert(0, partial_sig);
        }
    }

    Ok(StatusCode::OK)
}

// POST /session/:id/partial
#[instrument(skip(state))]
async fn submit_partial_signature(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(sig_pair): Json<PartialSignaturePair>,
) -> Result<StatusCode, StatusCode> {
    let decoded_sig = base64::engine::general_purpose::STANDARD
        .decode(&sig_pair.signature)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let partial_sig =
        PartialSignature::from_slice(&decoded_sig).map_err(|_| StatusCode::BAD_REQUEST)?;

    {
        let mut sessions = state.signing_sessions.write().await;
        let session = sessions.get_mut(&session_id).ok_or(StatusCode::NOT_FOUND)?;

        session
            .received_sigs
            .insert(sig_pair.signer_index, partial_sig);

        if session.received_sigs.len() == state.num_signers {
            let mut signer = state.signer.write().await;
            for (&i, psig) in &session.received_sigs {
                if i != 0 {
                    signer
                        .receive_partial_signature(i, *psig)
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                }
            }

            let final_sig = signer
                .create_final_signature()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            session.final_signature = Some(final_sig);

            // Verify final sig
            let agg_pub = signer.aggregated_pubkey();
            verify_signature(agg_pub, final_sig, &session.message)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }

    Ok(StatusCode::OK)
}

// GET /session/:id/signature
#[instrument(skip(state))]
async fn get_final_signature(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<String>, StatusCode> {
    let sessions = state.signing_sessions.read().await;
    let session = sessions.get(&session_id).ok_or(StatusCode::NOT_FOUND)?;

    if let Some(final_sig) = &session.final_signature {
        Ok(Json(hex::encode(final_sig.serialize())))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn create_signing_session(state: &AppState) -> anyhow::Result<SigningSessionResponse> {
    let unsigned_tx = {
        let transaction_builder = create_test_withdrawal_builder(&state.bridge_address).await?;
        let withdrawals = create_test_withdrawal_requests()?;
        let proof_txid = Txid::hash(&[0x42; 32]);

        let mut grouped_withdrawals: HashMap<Address, Amount> = HashMap::new();
        for w in withdrawals {
            *grouped_withdrawals.entry(w.address).or_insert(Amount::ZERO) = grouped_withdrawals
                .get(&w.address)
                .unwrap_or(&Amount::ZERO)
                .checked_add(w.amount)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
        }

        // Create outputs for grouped withdrawals
        let outputs: Vec<TxOut> = grouped_withdrawals
            .into_iter()
            .map(|(address, amount)| TxOut {
                value: amount,
                script_pubkey: address.script_pubkey(),
            })
            .collect();

        const OP_RETURN_WITHDRAW_PREFIX: &[u8] = b"VIA_PROTOCOL:WITHDRAWAL:";

        transaction_builder
            .build_transaction_with_op_return(
                outputs,
                OP_RETURN_WITHDRAW_PREFIX,
                vec![proof_txid.as_raw_hash().to_byte_array()],
            )
            .await?
    };

    // Create unique session ID
    let session_id = Uuid::new_v4().to_string();
    let tx_id = unsigned_tx.txid.to_string();

    {
        let mut utxos = state.unsigned_txs.write().await;
        utxos.insert(tx_id.clone(), unsigned_tx.clone());
    }

    // TODO: extract sighash and sign it and broadcast in last step
    let message = unsigned_tx.tx.compute_txid().as_byte_array().to_vec();
    {
        let mut signer = state.signer.write().await;
        signer.start_signing_session(message.clone())?;
    }

    let session = SigningSession {
        session_id: session_id.clone(),
        tx_id,
        received_nonces: HashMap::new(),
        received_sigs: HashMap::new(),
        final_signature: None,
        message: message.clone(),
    };

    {
        let mut sessions = state.signing_sessions.write().await;
        sessions.insert(session_id.clone(), session);
    }

    // Coordinator is signer_index 0, so insert coordinator's nonce:
    {
        #[allow(unused_mut)]
        let mut signer = state.signer.write().await;
        let coordinator_nonce = signer.our_nonce().expect("nonce should be generated");
        let mut sessions = state.signing_sessions.write().await;
        let session = sessions.get_mut(&session_id).unwrap();
        session.received_nonces.insert(0, coordinator_nonce);
    }

    let signer = state.signer.read().await;
    Ok(SigningSessionResponse {
        session_id,
        message_to_sign: hex::encode(message),
        aggregated_pubkey: hex::encode(signer.aggregated_pubkey().serialize()),
        required_signers: state.num_signers,
        received_nonces: 1,
        received_partial_signatures: 0,
        final_signature: None,
    })
}

// Mock a TransactionBuilder
async fn create_test_withdrawal_builder(
    bridge_address: &Address,
) -> anyhow::Result<TransactionBuilder> {
    let rpc_url = "http://localhost:18443";
    let network = BitcoinNetwork::Regtest;
    let auth = bitcoincore_rpc::Auth::None;
    let btc_client = Arc::new(BitcoinClient::new(rpc_url, network, auth).unwrap());

    let builder = TransactionBuilder::new(btc_client, bridge_address.clone())?;
    Ok(builder)
}

// Mock withdrawal requests
fn create_test_withdrawal_requests() -> anyhow::Result<Vec<WithdrawalRequest>> {
    // Just create two withdrawal requests for demonstration
    let addr1 =
        Address::from_str("bcrt1pv6dtdf0vrrj6ntas926v8vw9u0j3mga29vmfnxh39zfxya83p89qz9ze3l")?
            .require_network(Network::Regtest)?;
    let addr2 = Address::from_str("bcrt1qxyzxyzxyzxyzxyzxyzxyzxyzxyzxyzxyzxyzxyzxyz0abcd")?
        .require_network(Network::Regtest)?;

    let requests = vec![
        WithdrawalRequest {
            address: addr1,
            amount: Amount::from_btc(0.1)?,
        },
        WithdrawalRequest {
            address: addr2,
            amount: Amount::from_btc(0.05)?,
        },
    ];
    Ok(requests)
}

async fn get_nonces(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<HashMap<usize, String>>, StatusCode> {
    let sessions = state.signing_sessions.read().await;
    let session = sessions.get(&session_id).ok_or(StatusCode::NOT_FOUND)?;

    let mut nonces = HashMap::new();
    for (&idx, nonce) in &session.received_nonces {
        nonces.insert(
            idx,
            base64::engine::general_purpose::STANDARD.encode(nonce.to_bytes()),
        );
    }

    Ok(Json(nonces))
}
