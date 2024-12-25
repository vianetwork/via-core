use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use base64::Engine;
use musig2::{BinaryEncoding, PubNonce};
use reqwest::{Client, StatusCode};
use tokio::sync::watch;
use via_btc_client::traits::BitcoinOps;
use via_musig2::Signer;
use zksync_config::configs::via_verifier::ViaVerifierConfig;

use crate::{
    types::{NoncePair, PartialSignaturePair, SigningSessionResponse},
    utils::get_signer,
};

pub struct ViaWithdrawalVerifier {
    btc_client: Arc<dyn BitcoinOps>,
    config: ViaVerifierConfig,
    client: Client,
    signer: Signer,
}

impl ViaWithdrawalVerifier {
    pub async fn new(
        btc_client: Arc<dyn BitcoinOps>,
        config: ViaVerifierConfig,
    ) -> anyhow::Result<Self> {
        let signer = get_signer(
            &config.private_key.clone(),
            config.verifiers_pub_keys_str.clone(),
        )?;

        Ok(Self {
            btc_client,
            signer,
            client: Client::new(),
            config,
        })
    }

    pub async fn run(mut self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut timer = tokio::time::interval(self.config.polling_interval());

        while !*stop_receiver.borrow_and_update() {
            tokio::select! {
                _ = timer.tick() => { /* continue iterations */ }
                _ = stop_receiver.changed() => break,
            }

            match self.loop_iteration().await {
                Ok(()) => {
                    tracing::info!("Verifier withdrawal task finished");
                }
                Err(err) => {
                    tracing::error!("Failed to process verifier withdrawal task: {err}");
                }
            }
        }

        tracing::info!("Stop signal received, verifier withdrawal is shutting down");
        Ok(())
    }

    async fn loop_iteration(&mut self) -> Result<(), anyhow::Error> {
        let session_info = self.get_session().await?;

        // Check if the final signature was created
        if session_info.final_signature.is_some() {
            return Ok(());
        }

        let session_signature_status = self.get_session_signatures_status().await?;
        let session_nonces = self.get_session_nonces().await?;
        let verifier_index = self.signer.signer_index();

        if session_signature_status.get(&verifier_index).is_some()
            && session_nonces.get(&verifier_index).is_some()
        {
            // The verifier already sent his nonce and partial signature
            return Ok(());
        }

        // Reinit the signer incase the coordinator lost his in memory data
        if session_signature_status.get(&verifier_index).is_none()
            && session_nonces.get(&verifier_index).is_none()
            && (self.signer.has_created_partial_sig() || self.signer.has_submitted_nonce())
        {
            _ = self.reinit_signer();
        }

        if session_info.received_nonces < session_info.required_signers {
            let message = hex::decode(&session_info.message_to_sign)?;

            if self.signer.has_not_started() {
                self.signer.start_signing_session(message)?;
            }

            if session_nonces.get(&verifier_index).is_none() {
                self.submit_nonce().await?;
            }
        } else if session_info.received_partial_signatures < session_info.required_signers {
            if self.signer.has_created_partial_sig() {
                return Ok(());
            }

            self.send_partial_signature(session_nonces).await?;
        }

        Ok(())
    }

    async fn get_session(&self) -> anyhow::Result<SigningSessionResponse> {
        let url = format!("{}/session/", self.config.bind_addr());
        let resp = self.client.get(&url).send().await?;
        if resp.status().as_u16() != StatusCode::OK.as_u16() {
            anyhow::bail!("Error to fetch the session");
        }
        let session_info: SigningSessionResponse = resp.json().await?;
        Ok(session_info)
    }

    async fn get_session_nonces(&self) -> anyhow::Result<HashMap<usize, String>> {
        // We need to fetch all nonces from the coordinator
        let nonces_url = format!("{}/session/nonce/", self.config.bind_addr());
        let resp = self.client.get(&nonces_url).send().await?;
        let nonces: HashMap<usize, String> = resp.json().await?;
        Ok(nonces)
    }

    async fn submit_nonce(&mut self) -> anyhow::Result<()> {
        let nonce = self
            .signer
            .our_nonce()
            .ok_or_else(|| anyhow::anyhow!("No nonce available"))?;
        let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(nonce.to_bytes());
        let nonce_pair = NoncePair {
            signer_index: self.signer.signer_index(),
            nonce: nonce_b64,
        };
        let nonce_url = format!("{}/session/nonce/", self.config.bind_addr());
        let res = self
            .client
            .post(&nonce_url)
            .json(&nonce_pair)
            .send()
            .await?;

        if res.status().is_success() {
            self.signer.mark_nonce_submitted();
            return Ok(());
        }
        Ok(())
    }

    async fn get_session_signatures_status(&self) -> anyhow::Result<HashMap<usize, bool>> {
        // We need to fetch all nonces from the coordinator
        let url = format!("{}/session/signature/", self.config.bind_addr());
        let resp = self.client.get(&url).send().await?;
        let signatures: HashMap<usize, bool> = resp.json().await?;
        Ok(signatures)
    }

    async fn send_partial_signature(
        &mut self,
        session_nonces: HashMap<usize, String>,
    ) -> anyhow::Result<()> {
        // Process each nonce
        for (idx, nonce_b64) in session_nonces {
            if idx != self.signer.signer_index() {
                let nonce_bytes = base64::engine::general_purpose::STANDARD.decode(nonce_b64)?;
                let nonce = PubNonce::from_bytes(&nonce_bytes)?;
                self.signer
                    .receive_nonce(idx, nonce.clone())
                    .map_err(|e| anyhow::anyhow!("Failed to receive nonce: {}", e))?;
            }
        }

        let partial_sig = self.signer.create_partial_signature()?;
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(partial_sig.serialize());
        let sig_pair = PartialSignaturePair {
            signer_index: self.signer.signer_index(),
            signature: sig_b64,
        };

        let url = format!("{}/session/signature/", self.config.bind_addr(),);
        let resp = self.client.post(&url).json(&sig_pair).send().await?;
        if resp.status().is_success() {
            self.signer.mark_partial_sig_submitted();
        }
        Ok(())
    }

    fn reinit_signer(&mut self) -> anyhow::Result<()> {
        let signer = get_signer(
            &self.config.private_key.clone(),
            self.config.verifiers_pub_keys_str.clone(),
        )?;
        self.signer = signer;
        Ok(())
    }

    async fn create_new_session(&mut self) -> anyhow::Result<()> {
        let url = format!("{}/session/new/", self.config.bind_addr(),);
        let resp = self.client.post(&url).send().await?;
        if !resp.status().is_success() {}
        Ok(())
    }

    async fn broadcast_transaction(&mut self) -> anyhow::Result<()> {
        let session_info = self.get_session().await?;
        if let Some(signature) = session_info.final_signature {
            let txid = self
                .btc_client
                .broadcast_signed_transaction(&signature)
                .await?;
            // Todo: store in db
        }
        Ok(())
    }
}
