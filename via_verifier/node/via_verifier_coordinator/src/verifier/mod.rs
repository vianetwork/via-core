use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Result;
use bitcoin::{Address, TapSighashType, Witness};
use musig2::{CompactSignature, PartialSignature};
use reqwest::{header, Client, StatusCode};
use tokio::sync::watch;
use via_btc_client::traits::{BitcoinOps, Serializable};
use via_musig2::{transaction_builder::TransactionBuilder, verify_signature, Signer};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use via_verifier_types::{protocol_version::get_sequencer_version, transaction::UnsignedBridgeTx};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::configs::{via_verifier::ViaVerifierConfig, via_wallets::ViaWallet};
use zksync_types::via_roles::ViaNodeRole;
use zksync_utils::time::seconds_since_epoch;

use crate::{
    metrics::METRICS,
    sessions::{session_manager::SessionManager, withdrawal::WithdrawalSession},
    traits::ISession,
    types::{
        NoncePair, PartialSignaturePair, SessionOperation, SessionType, SigningSessionResponse,
    },
    utils::{decode_nonce, decode_signature, encode_nonce, encode_signature, get_signer},
};

pub struct ViaWithdrawalVerifier {
    verifier_config: ViaVerifierConfig,
    wallet: ViaWallet,
    session_manager: SessionManager,
    btc_client: Arc<dyn BitcoinOps>,
    master_connection_pool: ConnectionPool<Verifier>,
    client: Client,
    signer: Signer,
    final_sig: Option<CompactSignature>,
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
        let signer = get_signer(&wallet.private_key, verifiers_pub_keys.clone())?;

        let transaction_builder =
            Arc::new(TransactionBuilder::new(btc_client.clone(), bridge_address)?);

        let withdrawal_session = WithdrawalSession::new(
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
            signer,
            final_sig: None,
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

            if self
                .build_and_broadcast_final_transaction(&session_info, &session_op)
                .await?
            {
                return Ok(());
            }
        }

        if session_info.session_op.is_empty() {
            tracing::debug!("Empty session, nothing to process");
            return Ok(());
        }
        let session_op = SessionOperation::from_bytes(&session_info.session_op);

        let session_signature = self.get_session_signatures().await?;
        let session_nonces = self.get_session_nonces().await?;
        let verifier_index = self.signer.signer_index();

        if session_signature.contains_key(&verifier_index)
            && session_nonces.contains_key(&verifier_index)
        {
            return Ok(());
        }

        // Reinit the signer, when a new session is created by the coordinator.
        if !session_signature.contains_key(&verifier_index)
            && !session_nonces.contains_key(&verifier_index)
            && (self.signer.has_created_partial_sig() || self.signer.has_submitted_nonce())
        {
            self.reinit_signer()?;
            return Ok(());
        }

        if session_info.received_nonces < session_info.required_signers {
            if !self.session_manager.verify_message(&session_op).await? {
                METRICS
                    .session_invalid_message
                    .set(session_op.get_l1_batche_number() as usize);
                anyhow::bail!("Error when verify the session message");
            }

            if self.signer.has_not_started() {
                self.signer
                    .start_signing_session(session_op.get_message_to_sign())?;
            }

            if !session_nonces.contains_key(&verifier_index) {
                self.submit_nonce().await?;
            }
        } else if session_info.received_nonces >= session_info.required_signers {
            if self.signer.has_created_partial_sig() {
                return Ok(());
            }
            self.submit_partial_signature(session_nonces).await?;

            METRICS
                .session_last_valid_session
                .set(session_op.get_l1_batche_number() as usize);
        }

        Ok(())
    }

    fn create_request_headers(&self) -> anyhow::Result<header::HeaderMap> {
        let mut headers = header::HeaderMap::new();
        let timestamp = chrono::Utc::now().timestamp().to_string();
        let verifier_index = self.signer.signer_index().to_string();
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

    async fn get_session_nonces(&self) -> anyhow::Result<HashMap<usize, String>> {
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
        let nonces: HashMap<usize, String> = resp.json().await?;
        Ok(nonces)
    }

    async fn submit_nonce(&mut self) -> anyhow::Result<()> {
        let nonce = self
            .signer
            .our_nonce()
            .ok_or_else(|| anyhow::anyhow!("No nonce available"))?;

        tracing::debug!("Start submit nonce");

        let nonce_pair = encode_nonce(self.signer.signer_index(), nonce).unwrap();
        let url = format!(
            "{}/session/nonce",
            self.verifier_config.coordinator_http_url
        );
        let headers = self.create_request_headers()?;
        let res = self
            .client
            .post(&url)
            .headers(headers.clone())
            .json(&nonce_pair)
            .send()
            .await?;

        if res.status().is_success() {
            self.signer.mark_nonce_submitted();

            tracing::debug!("Nonce submitted successfully");
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to submit nonce, response: {}, url: {}, headers: {:?}, body: {:?} ",
                res.text().await?,
                url,
                headers,
                nonce_pair
            );
        }
    }

    async fn get_session_signatures(&self) -> anyhow::Result<HashMap<usize, PartialSignature>> {
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

        if resp.status().as_u16() != StatusCode::OK.as_u16() {
            anyhow::bail!(
                "Error to fetch the session signatures, status: {}, url: {}, headers: {:?}, resp: {:?}",
                resp.status(),
                url,
                headers,
                resp.text().await?
            );
        }
        let signatures: HashMap<usize, PartialSignaturePair> = resp.json().await?;
        let mut partial_sigs: HashMap<usize, PartialSignature> = HashMap::new();
        for (idx, sig) in signatures {
            partial_sigs.insert(idx, decode_signature(sig.signature).unwrap());
        }
        Ok(partial_sigs)
    }

    async fn submit_partial_signature(
        &mut self,
        session_nonces: HashMap<usize, String>,
    ) -> anyhow::Result<()> {
        // Process each nonce
        for (idx, nonce_b64) in session_nonces {
            if idx != self.signer.signer_index() {
                let nonce = decode_nonce(NoncePair {
                    signer_index: idx,
                    nonce: nonce_b64,
                })?;
                self.signer
                    .receive_nonce(idx, nonce.clone())
                    .map_err(|e| anyhow::anyhow!("Failed to receive nonce: {}", e))?;
            }
        }

        tracing::debug!("Start submit partial signature");

        let partial_sig = self.signer.create_partial_signature()?;
        let sig_pair = encode_signature(self.signer.signer_index(), partial_sig)?;

        let url = format!(
            "{}/session/signature",
            self.verifier_config.coordinator_http_url
        );
        let headers = self.create_request_headers()?;
        let resp = self
            .client
            .post(&url)
            .headers(headers.clone())
            .json(&sig_pair)
            .send()
            .await?;
        if resp.status().is_success() {
            self.signer.mark_partial_sig_submitted();
            tracing::debug!("Partial signature submitted");
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to submit partial signature, response: {}, url: {}, headers: {:?}, body: {:?} ",
                resp.text().await?,
                url,
                headers,
                sig_pair
            );
        }
    }

    fn reinit_signer(&mut self) -> anyhow::Result<()> {
        let signer = get_signer(
            &self.wallet.private_key.clone(),
            self.verifiers_pub_keys.clone(),
        )?;
        self.signer = signer;
        self.final_sig = None;
        Ok(())
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
                "Failed to create a new session, response: {}, url: {}, headers: {:?}",
                resp.text().await?,
                url,
                headers
            );
            self.reinit_signer()?;
        }
        Ok(())
    }

    async fn create_final_signature(&mut self, message: &[u8]) -> anyhow::Result<()> {
        if self.final_sig.is_some() {
            return Ok(());
        }

        let signatures = self.get_session_signatures().await?;
        for (&i, sig) in &signatures {
            if self.signer.signer_index() != i {
                self.signer.receive_partial_signature(i, *sig)?;
            }
        }

        let final_sig = self.signer.create_final_signature()?;
        tracing::debug!("Partial signature created");

        let agg_pub = self.signer.aggregated_pubkey();
        verify_signature(agg_pub, final_sig, message)?;
        self.final_sig = Some(final_sig);

        tracing::debug!("Partial signature verified");

        Ok(())
    }

    fn sign_transaction(
        &self,
        unsigned_tx: UnsignedBridgeTx,
        musig2_signature: CompactSignature,
    ) -> String {
        let mut unsigned_tx = unsigned_tx;
        let mut final_sig_with_hashtype = musig2_signature.serialize().to_vec();
        let sighash_type = TapSighashType::All;
        final_sig_with_hashtype.push(sighash_type as u8);
        for tx in &mut unsigned_tx.tx.input {
            tx.witness = Witness::from(vec![final_sig_with_hashtype.clone()]);
        }
        bitcoin::consensus::encode::serialize_hex(&unsigned_tx.tx)
    }

    async fn build_and_broadcast_final_transaction(
        &mut self,
        session_info: &SigningSessionResponse,
        session_op: &SessionOperation,
    ) -> anyhow::Result<bool> {
        if session_info.received_partial_signatures < session_info.required_signers {
            return Ok(false);
        }

        if let Some((unsigned_tx, message)) = session_op.session() {
            self.create_final_signature(message)
                .await
                .map_err(|e| anyhow::format_err!("Error create final signature: {e}"))?;

            if let Some(musig2_signature) = self.final_sig {
                if !self
                    .session_manager
                    .before_broadcast_final_transaction(session_op)
                    .await?
                {
                    return Ok(false);
                }

                let signed_tx = self.sign_transaction(unsigned_tx.clone(), musig2_signature);

                tracing::debug!("Session Info {:?}", &session_info);
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

                self.reinit_signer()?;

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
