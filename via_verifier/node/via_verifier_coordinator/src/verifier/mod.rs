use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use bitcoin::{Address, TapSighashType, Witness};
use musig2::{CompactSignature, PartialSignature};
use reqwest::{header, Client, StatusCode};
use tokio::sync::watch;
use via_btc_client::traits::{BitcoinOps, Serializable};
use via_musig2::{get_signer, transaction_builder::TransactionBuilder, verify_signature, Signer};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::{protocol_version::get_sequencer_version, transaction::UnsignedBridgeTx};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::configs::{via_verifier::ViaVerifierConfig, via_wallets::ViaWallet};
use zksync_types::{via_roles::ViaNodeRole, H256};
use zksync_utils::time::seconds_since_epoch;

use crate::{
    metrics::METRICS,
    sessions::{session_manager::SessionManager, withdrawal::WithdrawalSession},
    traits::ISession,
    types::{
        NoncePair, PartialSignaturePair, SessionOperation, SessionType, SigningSessionResponse,
    },
    utils::{decode_nonce, decode_signature, encode_nonce, encode_signature},
};

pub struct ViaWithdrawalVerifier {
    verifier_config: ViaVerifierConfig,
    wallet: ViaWallet,
    session_manager: SessionManager,
    btc_client: Arc<dyn BitcoinOps>,
    master_connection_pool: ConnectionPool<Verifier>,
    client: Client,
    signer_per_utxo_input: BTreeMap<usize, Signer>,
    final_sig_per_utxo_input: BTreeMap<usize, CompactSignature>,
    verifiers_pub_keys: Vec<String>,
}

impl ViaWithdrawalVerifier {
    pub fn new(
        verifier_config: ViaVerifierConfig,
        wallet: ViaWallet,
        master_connection_pool: ConnectionPool<Verifier>,
        btc_client: Arc<dyn BitcoinOps>,
        withdrawal_client: WithdrawalClient,
        bridge_address: Address,
        verifiers_pub_keys: Vec<String>,
    ) -> anyhow::Result<Self> {
        let transaction_builder =
            Arc::new(TransactionBuilder::new(btc_client.clone(), bridge_address)?);

        let withdrawal_session = WithdrawalSession::new(
            verifier_config.clone(),
            master_connection_pool.clone(),
            transaction_builder.clone(),
            withdrawal_client,
        );

        // Add sessions type the verifier network can process
        let sessions: HashMap<SessionType, Arc<dyn ISession>> = [(
            SessionType::Withdrawal,
            Arc::new(withdrawal_session) as Arc<dyn ISession>,
        )]
        .into_iter()
        .collect();

        Ok(Self {
            verifier_config,
            wallet,
            session_manager: SessionManager::new(sessions),
            btc_client,
            master_connection_pool,
            client: Client::new(),
            signer_per_utxo_input: BTreeMap::new(),
            final_sig_per_utxo_input: BTreeMap::new(),
            verifiers_pub_keys,
        })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.verifier_config.polling_interval());

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            match self.loop_iteration().await {
                Ok(()) => {}
                Err(err) => {
                    tracing::error!("Failed to process verifier withdrawal task: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, verifier withdrawal is shutting down");
        Ok(())
    }
    async fn loop_iteration(&mut self) -> Result<(), anyhow::Error> {
        if self.sync_in_progress().await? {
            return Ok(());
        }

        let mut session_info = self.get_session().await?;

        if self.is_coordinator() {
            self.create_new_session().await?;
            session_info = self.get_session().await?;

            if session_info.session_op.is_empty() {
                tracing::debug!("Empty session, nothing to process");
                return Ok(());
            }

            let session_op = SessionOperation::from_bytes(&session_info.session_op);

            if !self
                .session_manager
                .before_process_session(&session_op)
                .await?
            {
                tracing::debug!("Empty session, nothing to process");
                return Ok(());
            }
        }

        if session_info.session_op.is_empty() {
            tracing::debug!("Empty session, nothing to process");
            return Ok(());
        }
        let session_op = SessionOperation::from_bytes(&session_info.session_op);

        if self
            .build_and_broadcast_final_transaction(&session_info, &session_op)
            .await?
        {
            return Ok(());
        }

        let messages = session_op.get_message_to_sign();

        // The verifier checks if the session is sequential.
        if session_op.get_session_type() == SessionType::Withdrawal {
            let session = self
                .session_manager
                .sessions
                .get(&session_op.get_session_type())
                .ok_or_else(|| anyhow!("Failed to get session withdrawal"))?
                .clone();

            let withdrawal_session = session
                .as_ref()
                .as_any()
                .downcast_ref::<WithdrawalSession>()
                .ok_or_else(|| anyhow!("Failed to cast to WithdrawalSession"))?;

            let (expected_l1_batch_number, _, _) =
                withdrawal_session.prepare_withdrawal_session().await?;

            let actual_batch_number = session_op.get_l1_batch_number();
            if expected_l1_batch_number > 0 && actual_batch_number != expected_l1_batch_number {
                tracing::warn!(
                    "Withdrawal session not yet sequential: last processed {}, found {}",
                    expected_l1_batch_number,
                    actual_batch_number
                );
                return Ok(());
            }
        }

        if self
            .session_manager
            .is_bridge_session_already_processed(&session_op)
            .await?
        {
            tracing::info!(
                "Session already processed l1_batch_number {}",
                session_op.get_l1_batch_number()
            );
            return Ok(());
        }

        if self.signer_per_utxo_input.len() < messages.len() {
            self.init_signers(messages.len())?;
        }

        let input_index = 0;

        let session_signatures = self.get_session_signatures().await?;
        let session_nonces = self.get_session_nonces().await?;

        let signer = match self.signer_per_utxo_input.get_mut(&input_index) {
            Some(signer) => signer,
            None => {
                tracing::warn!("No signer found for input index {input_index}");
                return Ok(());
            }
        };

        let already_signed = session_signatures
            .get(&input_index)
            .map_or(false, |map| map.contains_key(&signer.signer_index()));

        let already_sent_nonce = session_nonces
            .get(&input_index)
            .map_or(false, |map| map.contains_key(&signer.signer_index()));

        if already_signed && already_sent_nonce {
            return Ok(());
        }

        if !already_signed
            && !already_sent_nonce
            && (signer.has_created_partial_sig() || signer.has_submitted_nonce())
        {
            self.clear_signers();
            return Ok(());
        }

        let received_nonces = session_nonces.get(&input_index).map_or(0, |map| map.len());
        if received_nonces < session_info.required_signers {
            if !self.session_manager.verify_message(&session_op).await? {
                METRICS
                    .session_invalid_message
                    .set(session_op.get_l1_batch_number() as usize);
                anyhow::bail!("Invalid session message");
            }

            // If the session is valid but there is no withdrawal to process, insert and empty hash.
            if session_op.get_unsigned_bridge_tx().is_empty() {
                let votable_tx_id = self
                    .master_connection_pool
                    .connection_tagged("verifier task")
                    .await?
                    .via_votes_dal()
                    .get_votable_transaction_id(&session_op.get_proof_tx_id())
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Votable transaction does not exist"))?;

                self.master_connection_pool
                    .connection_tagged("verifier task")
                    .await?
                    .via_bridge_dal()
                    .update_bridge_tx(votable_tx_id, 0, &H256::zero().as_bytes())
                    .await?;

                return Ok(());
            }

            if !already_sent_nonce {
                self.submit_nonce(messages).await?;
            }
        } else if received_nonces >= session_info.required_signers {
            if signer.has_created_partial_sig() {
                return Ok(());
            }

            self.submit_partial_signature(session_nonces).await?;

            METRICS
                .session_last_valid_session
                .set(session_op.get_l1_batch_number() as usize);
        }

        Ok(())
    }

    fn create_request_headers(&self) -> anyhow::Result<header::HeaderMap> {
        let mut headers = header::HeaderMap::new();
        let timestamp = chrono::Utc::now().timestamp().to_string();
        let signer = get_signer(&self.wallet.private_key, self.verifiers_pub_keys.clone())?;
        let verifier_index = signer.signer_index().to_string();
        let sequencer_version = get_sequencer_version().to_string();

        let private_key = bitcoin::PrivateKey::from_wif(&self.wallet.private_key)?;
        let secret_key = private_key.inner;

        // Sign timestamp + verifier_index + sequencer_version as a JSON object
        let payload = serde_json::json!({
            "timestamp": timestamp,
            "verifier_index": verifier_index,
            "sequencer_version": sequencer_version
        });
        let signature = crate::auth::sign_request(&payload, &secret_key)?;

        headers.insert("X-Timestamp", header::HeaderValue::from_str(&timestamp)?);
        headers.insert(
            "X-Verifier-Index",
            header::HeaderValue::from_str(&verifier_index)?,
        );
        headers.insert("X-Signature", header::HeaderValue::from_str(&signature)?);
        headers.insert(
            "X-Sequencer-Version",
            header::HeaderValue::from_str(&sequencer_version)?,
        );

        Ok(headers)
    }

    async fn get_session(&self) -> anyhow::Result<SigningSessionResponse> {
        let url = format!("{}/session", self.verifier_config.coordinator_http_url);
        let headers = self.create_request_headers()?;
        let resp = self
            .client
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;
        if resp.status().as_u16() != StatusCode::OK.as_u16() {
            anyhow::bail!(
                "Error to fetch the session, status: {}, url: {}, headers: {:?}, resp: {:?}",
                resp.status(),
                url,
                headers,
                resp.text().await?
            );
        }
        let session_info: SigningSessionResponse = resp.json().await?;
        Ok(session_info)
    }

    async fn get_session_nonces(&self) -> anyhow::Result<BTreeMap<usize, BTreeMap<usize, String>>> {
        let nonces_url = format!(
            "{}/session/nonce",
            self.verifier_config.coordinator_http_url
        );
        let headers = self.create_request_headers()?;
        let resp = self
            .client
            .get(&nonces_url)
            .headers(headers.clone())
            .send()
            .await?;

        if resp.status().as_u16() != StatusCode::OK.as_u16() {
            anyhow::bail!(
                "Error to fetch the session nonces, status: {}, url: {}, headers: {:?}, resp: {:?}",
                resp.status(),
                nonces_url,
                headers,
                resp.text().await?
            );
        }
        let nonces: BTreeMap<usize, BTreeMap<usize, String>> = resp.json().await?;
        Ok(nonces)
    }

    pub async fn submit_nonce(&mut self, messages: Vec<Vec<u8>>) -> anyhow::Result<()> {
        let mut nonce_map: BTreeMap<usize, NoncePair> = BTreeMap::new();

        for (input_index, signer) in self.signer_per_utxo_input.iter_mut() {
            if signer.has_not_started() {
                signer.start_signing_session(messages[*input_index].clone())?;
            }

            let nonce = signer
                .our_nonce()
                .ok_or_else(|| anyhow::anyhow!("No nonce available for input {}", input_index))?;

            let nonce_pair = encode_nonce(signer.signer_index(), nonce)
                .map_err(|e| anyhow::anyhow!("Failed to encode nonce: {}", e))?;

            nonce_map.insert(*input_index, nonce_pair);
        }

        let url = format!(
            "{}/session/nonce",
            self.verifier_config.coordinator_http_url
        );
        let headers = self.create_request_headers()?;

        let res = self
            .client
            .post(&url)
            .headers(headers.clone())
            .json(&nonce_map)
            .send()
            .await?;

        if res.status().is_success() {
            for (_, signer) in self.signer_per_utxo_input.iter_mut() {
                signer.mark_nonce_submitted();
            }

            tracing::debug!("All nonces submitted successfully");
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to submit nonce map. Status: {}, URL: {}, Headers: {:?}, Response: {}",
                res.status(),
                url,
                headers,
                res.text().await?
            );
        }
    }

    pub async fn get_session_signatures(
        &self,
    ) -> anyhow::Result<BTreeMap<usize, BTreeMap<usize, PartialSignature>>> {
        let url = format!(
            "{}/session/signature",
            self.verifier_config.coordinator_http_url
        );
        let headers = self.create_request_headers()?;
        let resp = self
            .client
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;

        if resp.status() != StatusCode::OK {
            anyhow::bail!(
                "Error fetching session signatures. Status: {}, URL: {}, Headers: {:?}, Body: {}",
                resp.status(),
                url,
                headers,
                resp.text().await?
            );
        }

        let raw_sigs: BTreeMap<usize, Vec<PartialSignaturePair>> = resp.json().await?;
        let mut decoded_sigs: BTreeMap<usize, BTreeMap<usize, PartialSignature>> = BTreeMap::new();

        for (input_index, sigs_per_signer) in raw_sigs {
            let mut inner_map = BTreeMap::new();

            for encoded_sig in sigs_per_signer {
                let sig = decode_signature(encoded_sig.signature).with_context(|| {
                    format!(
                        "Failed to decode signature for input {} signer {}",
                        input_index, encoded_sig.signer_index
                    )
                })?;
                inner_map.insert(encoded_sig.signer_index, sig);
            }

            decoded_sigs.insert(input_index, inner_map);
        }

        Ok(decoded_sigs)
    }

    pub async fn submit_partial_signature(
        &mut self,
        session_nonces: BTreeMap<usize, BTreeMap<usize, String>>,
    ) -> anyhow::Result<()> {
        let mut sig_pair_per_input = BTreeMap::new();

        for (input_index, nonces) in session_nonces {
            let signer = self
                .signer_per_utxo_input
                .get_mut(&input_index)
                .ok_or_else(|| anyhow::anyhow!("Missing signer for input index {}", input_index))?;

            for (signer_index, nonce_b64) in nonces {
                if signer_index != signer.signer_index() {
                    let nonce = decode_nonce(NoncePair {
                        signer_index,
                        nonce: nonce_b64,
                    })
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to decode or parse nonce for signer {}: {}",
                            signer_index,
                            e
                        )
                    })?;

                    signer.receive_nonce(signer_index, nonce).map_err(|e| {
                        anyhow::anyhow!(
                            "Signer {} failed to receive nonce from {}: {}",
                            input_index,
                            signer_index,
                            e
                        )
                    })?;
                }
            }

            tracing::info!("Creating partial signature for input {}", input_index);

            let partial_sig = signer.create_partial_signature().map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create partial signature for input {}: {}",
                    input_index,
                    e
                )
            })?;

            let encoded = encode_signature(signer.signer_index(), partial_sig).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to encode partial signature for input {}: {}",
                    input_index,
                    e
                )
            })?;

            sig_pair_per_input.insert(input_index, encoded);
        }

        let url = format!(
            "{}/session/signature",
            self.verifier_config.coordinator_http_url
        );
        let headers = self.create_request_headers()?;

        tracing::debug!("Submitting all partial signatures to {}", url);

        let response = self
            .client
            .post(&url)
            .headers(headers.clone())
            .json(&sig_pair_per_input)
            .send()
            .await?;

        if response.status().is_success() {
            for input_index in sig_pair_per_input.keys() {
                if let Some(signer) = self.signer_per_utxo_input.get_mut(input_index) {
                    signer.mark_partial_sig_submitted();
                }
            }

            tracing::debug!("Partial signatures submitted successfully");
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();

            anyhow::bail!(
            "Failed to submit partial signatures. Status: {}, Body: {}, URL: {}, Headers: {:?}, Payload: {:?}",
            status,
            body,
            url,
            headers,
            sig_pair_per_input
        );
        }
    }

    fn init_signers(&mut self, count: usize) -> anyhow::Result<()> {
        self.clear_signers();

        for i in 0..count {
            self.signer_per_utxo_input.insert(
                i,
                get_signer(&self.wallet.private_key, self.verifiers_pub_keys.clone())?,
            );
        }
        Ok(())
    }

    fn clear_signers(&mut self) {
        self.signer_per_utxo_input.clear();
        self.final_sig_per_utxo_input = BTreeMap::new();
    }

    async fn create_new_session(&mut self) -> anyhow::Result<()> {
        let url = format!("{}/session/new", self.verifier_config.coordinator_http_url);
        let headers = self.create_request_headers()?;
        let resp = self
            .client
            .post(&url)
            .headers(headers.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::warn!(
                "Failed to create a new session, status: {}, response: {}, url: {}, headers: {:?}",
                resp.status().as_str(),
                resp.text().await?,
                url,
                headers
            );
            self.clear_signers();
        }
        Ok(())
    }

    pub async fn create_final_signature(&mut self, messages: &[Vec<u8>]) -> anyhow::Result<()> {
        if !self.final_sig_per_utxo_input.is_empty() {
            return Ok(());
        }

        let signatures = self.get_session_signatures().await?;
        let input_count = self.signer_per_utxo_input.len();

        if signatures.len() != input_count {
            anyhow::bail!(
                "Mismatch: expected signatures for {} inputs, but got {}",
                input_count,
                signatures.len()
            );
        }

        if messages.len() != input_count {
            anyhow::bail!(
                "Mismatch: expected messages for {} inputs, but got {}",
                input_count,
                messages.len()
            );
        }

        let mut final_sig_per_utxo_input = BTreeMap::new();

        for (input_index, sigs_per_signer) in &signatures {
            let signer = self
                .signer_per_utxo_input
                .get_mut(input_index)
                .ok_or_else(|| {
                    anyhow::anyhow!("No signer found for input index {}", input_index)
                })?;

            for (&signer_index, partial_sig) in sigs_per_signer {
                if signer.signer_index() != signer_index {
                    signer
                        .receive_partial_signature(signer_index, *partial_sig)
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "Error receiving partial signature (input {}, signer {}): {}",
                                input_index,
                                signer_index,
                                e
                            )
                        })?;
                }
            }

            let final_sig = signer.create_final_signature().map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create final signature (input {}): {}",
                    input_index,
                    e
                )
            })?;

            let message = messages.get(*input_index).ok_or_else(|| {
                anyhow::anyhow!("Missing message for input index {}", input_index)
            })?;

            verify_signature(signer.aggregated_pubkey(), final_sig, message).map_err(|e| {
                anyhow::anyhow!(
                    "Final signature verification failed (input {}): {}",
                    input_index,
                    e
                )
            })?;

            tracing::debug!(
                "Final signature created and verified for input {}",
                input_index
            );
            final_sig_per_utxo_input.insert(*input_index, final_sig);
        }

        self.final_sig_per_utxo_input = final_sig_per_utxo_input;
        Ok(())
    }

    fn sign_transaction(&self, unsigned_tx: UnsignedBridgeTx) -> String {
        let mut unsigned_tx = unsigned_tx;
        let sighash_type = TapSighashType::All;
        for (input_index, musig2_signature) in self.final_sig_per_utxo_input.clone() {
            let mut final_sig_with_hashtype = musig2_signature.serialize().to_vec();
            final_sig_with_hashtype.push(sighash_type as u8);
            unsigned_tx.tx.input[input_index].witness =
                Witness::from(vec![final_sig_with_hashtype.clone()]);
        }
        bitcoin::consensus::encode::serialize_hex(&unsigned_tx.tx)
    }

    async fn build_and_broadcast_final_transaction(
        &mut self,
        session_info: &SigningSessionResponse,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        let input_index = 0;
        let received_partial_signatures = session_info
            .received_partial_signatures
            .get(&input_index)
            .map_or(0, |len| *len);

        if received_partial_signatures < session_info.required_signers {
            return Ok(false);
        }

        if let Some((unsigned_tx, messages)) = session_op.session() {
            self.create_final_signature(messages)
                .await
                .map_err(|e| anyhow::format_err!("Error create final signature: {e}"))?;

            if !self.final_sig_per_utxo_input.is_empty() {
                if !self
                    .session_manager
                    .before_broadcast_final_transaction(session_op)
                    .await?
                {
                    return Ok(false);
                }

                let signed_tx = self.sign_transaction(unsigned_tx.clone());

                tracing::debug!("Signed transaction {:?}", &signed_tx);

                let txid = self
                    .btc_client
                    .broadcast_signed_transaction(&signed_tx)
                    .await?;

                tracing::info!(
                    "Broadcast {} signed transaction with txid {}",
                    &session_op.get_session_type(),
                    &txid.to_string()
                );

                if !self
                    .session_manager
                    .after_broadcast_final_transaction(txid, session_op)
                    .await?
                {
                    return Ok(false);
                }

                METRICS.session_time.observe(Duration::from_secs(
                    seconds_since_epoch() - session_info.created_at,
                ));

                self.clear_signers();

                return Ok(true);
            }
        }
        Ok(false)
    }

    fn is_coordinator(&self) -> bool {
        self.verifier_config.role == ViaNodeRole::Coordinator
    }

    async fn sync_in_progress(&self) -> anyhow::Result<bool> {
        let last_indexed_l1_block_number = self
            .master_connection_pool
            .connection_tagged("verifier task")
            .await?
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await?;
        let current_l1_block_number = self.btc_client.fetch_block_height().await?;

        if last_indexed_l1_block_number < current_l1_block_number {
            tracing::debug!(
                "The verifier synchronization in progress {}/{}",
                last_indexed_l1_block_number,
                current_l1_block_number
            );
            return Ok(true);
        }
        Ok(false)
    }
}
