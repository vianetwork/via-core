use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use bitcoin::{
    hashes::Hash,
    sighash::{Prevouts, SighashCache},
    Amount, TapSighashType, Txid, Witness,
};
use musig2::{CompactSignature, PartialSignature};
use reqwest::{header, Client, StatusCode};
use tokio::sync::watch;
use via_btc_client::{
    traits::{BitcoinOps, Serializable},
    withdrawal_builder::{UnsignedWithdrawalTx, WithdrawalBuilder},
};
use via_musig2::{verify_signature, Signer};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::configs::via_verifier::{VerifierMode, ViaVerifierConfig};
use zksync_types::H256;

use crate::{
    types::{NoncePair, PartialSignaturePair, SigningSessionResponse},
    utils::{
        decode_nonce, decode_signature, encode_nonce, encode_signature, get_signer, h256_to_txid,
    },
};

pub struct ViaWithdrawalVerifier {
    master_connection_pool: ConnectionPool<Verifier>,
    btc_client: Arc<dyn BitcoinOps>,
    config: ViaVerifierConfig,
    client: Client,
    withdrawal_client: WithdrawalClient,
    signer: Signer,
    final_sig: Option<CompactSignature>,
}

impl ViaWithdrawalVerifier {
    pub async fn new(
        master_connection_pool: ConnectionPool<Verifier>,
        btc_client: Arc<dyn BitcoinOps>,
        withdrawal_client: WithdrawalClient,
        config: ViaVerifierConfig,
    ) -> anyhow::Result<Self> {
        let signer = get_signer(
            &config.private_key.clone(),
            config.verifiers_pub_keys_str.clone(),
        )?;

        Ok(Self {
            master_connection_pool,
            btc_client,
            signer,
            client: Client::new(),
            withdrawal_client,
            config,
            final_sig: None,
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
        let mut session_info = self.get_session().await?;

        if self.config.verifier_mode == VerifierMode::COORDINATOR {
            tracing::info!("create a new session");

            if session_info.l1_block_number != 0 {
                let withdrawal_txid = self
                    .master_connection_pool
                    .connection_tagged("coordinator task")
                    .await?
                    .via_votes_dal()
                    .get_vote_transaction_withdrawal_tx(session_info.l1_block_number)
                    .await?;

                // TODO: refactore the transaction confirmation for the musig2, and implement utxo manager like in the inscriber
                // Check if the previous batch musig2 transaction was minted before start a new session.
                if let Some(tx) = withdrawal_txid {
                    let tx_id = Txid::from_slice(&tx)?;
                    let is_confirmed = self.btc_client.check_tx_confirmation(&tx_id, 1).await?;
                    if !is_confirmed {
                        return Ok(());
                    }
                }
            }

            self.create_new_session().await?;
        }

        session_info = self.get_session().await?;
        if session_info.l1_block_number == 0 {
            tracing::info!("Empty session, nothing to process");
            return Ok(());
        }

        if self.config.verifier_mode == VerifierMode::COORDINATOR {
            if self
                .build_and_broadcast_final_transaction(&session_info)
                .await?
            {
                return Ok(());
            }
        }

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
            let message = hex::decode(&session_info.message_to_sign)?;

            if !self.verify_message(&session_info).await? {
                anyhow::bail!("Error when verify the session message");
            }

            if self.signer.has_not_started() {
                self.signer.start_signing_session(message)?;
            }

            if !session_nonces.contains_key(&verifier_index) {
                self.submit_nonce().await?;
            }
        } else if session_info.received_nonces >= session_info.required_signers {
            if self.signer.has_created_partial_sig() {
                return Ok(());
            }
            self.submit_partial_signature(session_nonces).await?;
        }

        Ok(())
    }

    async fn get_session(&self) -> anyhow::Result<SigningSessionResponse> {
        let url = format!("{}/session", self.config.url);
        let resp = self.client.get(&url).send().await?;
        if resp.status().as_u16() != StatusCode::OK.as_u16() {
            anyhow::bail!("Error to fetch the session");
        }
        let session_info: SigningSessionResponse = resp.json().await?;
        Ok(session_info)
    }

    async fn verify_message(&self, session: &SigningSessionResponse) -> anyhow::Result<bool> {
        // Get the l1 batches finilized but withdrawals not yet processed
        if let Some((blob_id, proof_tx_id)) = self
            .master_connection_pool
            .connection_tagged("verifier")
            .await?
            .via_votes_dal()
            .get_finalized_block_and_non_processed_withdrawal(session.l1_block_number)
            .await?
        {
            if !self
                ._verify_withdrawals(&session, &blob_id, proof_tx_id)
                .await?
            {
                return Ok(false);
            }

            return self._verify_sighash(&session).await;
        }
        Ok(false)
    }
    async fn _verify_withdrawals(
        &self,
        session: &SigningSessionResponse,
        blob_id: &str,
        proof_tx_id: Vec<u8>,
    ) -> anyhow::Result<bool> {
        let withdrawals = self.withdrawal_client.get_withdrawals(blob_id).await?;
        let unsigned_tx = UnsignedWithdrawalTx::from_bytes(&session.unsigned_tx);

        // Group withdrawals by address and sum amounts
        let mut grouped_withdrawals: HashMap<String, Amount> = HashMap::new();
        for w in &withdrawals {
            let key = w.address.script_pubkey().to_string();
            *grouped_withdrawals.entry(key).or_insert(Amount::ZERO) = grouped_withdrawals
                .get(&key)
                .unwrap_or(&Amount::ZERO)
                .checked_add(w.amount)
                .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
        }

        let len = grouped_withdrawals.len();
        if len == 0 {
            tracing::error!(
                "Invalid session, there are no withdrawals to process, l1 batch: {}",
                session.l1_block_number
            );
            return Ok(false);
        }
        if len + 2 != unsigned_tx.tx.output.len() {
            // Log an error
            return Ok(false);
        }

        // Verify if all grouped_withdrawals are included with valid amount.
        for (i, txout) in unsigned_tx
            .tx
            .output
            .iter()
            .enumerate()
            .take(unsigned_tx.tx.output.len().saturating_sub(2))
        {
            let amount = &grouped_withdrawals[&txout.script_pubkey.to_string()];
            if amount != &txout.value {
                tracing::error!(
                    "Invalid request withdrawal for batch {}, index: {}",
                    session.l1_block_number,
                    i
                );
                return Ok(false);
            }
        }
        tracing::info!(
            "All request withdrawals for batch {} are valid",
            session.l1_block_number
        );

        // Verify the OP return
        let tx_id = h256_to_txid(&proof_tx_id)?;
        let op_return_data = WithdrawalBuilder::create_op_return_script(tx_id)?;
        let op_return_tx_out = &unsigned_tx.tx.output[unsigned_tx.tx.output.len() - 2];

        if op_return_tx_out.script_pubkey.to_string() != op_return_data.to_string()
            || op_return_tx_out.value != Amount::ZERO
        {
            tracing::error!(
                "Invalid op return data for l1 batch: {}",
                session.l1_block_number
            );
            return Ok(false);
        }

        Ok(true)
    }

    async fn _verify_sighash(&self, session: &SigningSessionResponse) -> anyhow::Result<bool> {
        // Verify the sighash
        let unsigned_tx = UnsignedWithdrawalTx::from_bytes(&session.unsigned_tx);
        let mut sighash_cache = SighashCache::new(&unsigned_tx.tx);

        let sighash_type = TapSighashType::All;
        let mut txout_list = Vec::with_capacity(unsigned_tx.utxos.len());

        for (_, txout) in unsigned_tx.utxos.clone() {
            txout_list.push(txout);
        }
        let sighash = sighash_cache
            .taproot_key_spend_signature_hash(0, &Prevouts::All(&txout_list), sighash_type)
            .context("Error taproot_key_spend_signature_hash")?;

        if session.message_to_sign != sighash.to_string() {
            tracing::error!(
                "Invalid transaction sighash for session with block id {}",
                session.l1_block_number
            );
            return Ok(false);
        }
        tracing::info!("Sighash for batch {} is valid", session.l1_block_number);
        Ok(true)
    }

    async fn get_session_nonces(&self) -> anyhow::Result<HashMap<usize, String>> {
        // We need to fetch all nonces from the coordinator
        let nonces_url = format!("{}/session/nonce", self.config.url);
        let resp = self.client.get(&nonces_url).send().await?;
        let nonces: HashMap<usize, String> = resp.json().await?;
        Ok(nonces)
    }

    async fn submit_nonce(&mut self) -> anyhow::Result<()> {
        let nonce = self
            .signer
            .our_nonce()
            .ok_or_else(|| anyhow::anyhow!("No nonce available"))?;

        let nonce_pair = encode_nonce(self.signer.signer_index(), nonce).unwrap();
        let url = format!("{}/session/nonce", self.config.url);
        let res = self.client.post(&url).json(&nonce_pair).send().await?;

        if res.status().is_success() {
            self.signer.mark_nonce_submitted();
            return Ok(());
        }
        Ok(())
    }

    async fn get_session_signatures(&self) -> anyhow::Result<HashMap<usize, PartialSignature>> {
        let url = format!("{}/session/signature", self.config.url);
        let resp = self.client.get(&url).send().await?;
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

        let partial_sig = self.signer.create_partial_signature()?;
        let sig_pair = encode_signature(self.signer.signer_index(), partial_sig)?;

        let url = format!("{}/session/signature", self.config.url,);
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
        self.final_sig = None;
        Ok(())
    }

    async fn create_new_session(&mut self) -> anyhow::Result<()> {
        let url = format!("{}/session/new", self.config.url);
        let resp = self
            .client
            .post(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            self.reinit_signer()?;
        }
        Ok(())
    }

    async fn create_final_signature(
        &mut self,
        session_info: &SigningSessionResponse,
    ) -> anyhow::Result<()> {
        if self.final_sig.is_some() {
            return Ok(());
        }

        if session_info.received_partial_signatures >= session_info.required_signers {
            let signatures = self.get_session_signatures().await?;
            for (&i, sig) in &signatures {
                if self.signer.signer_index() != i {
                    self.signer.receive_partial_signature(i, *sig)?;
                }
            }

            let final_sig = self.signer.create_final_signature()?;
            let agg_pub = self.signer.aggregated_pubkey();
            verify_signature(
                agg_pub,
                final_sig,
                &hex::decode(&session_info.message_to_sign)?,
            )?;
            self.final_sig = Some(final_sig);

            return Ok(());
        }
        Ok(())
    }

    fn sign_transaction(
        &self,
        unsigned_tx: UnsignedWithdrawalTx,
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
    ) -> anyhow::Result<bool> {
        self.create_final_signature(session_info)
            .await
            .context("Error create final signature")?;

        if let Some(musig2_signature) = self.final_sig {
            let withdrawal_txid = self
                .master_connection_pool
                .connection_tagged("coordinator task")
                .await?
                .via_votes_dal()
                .get_vote_transaction_withdrawal_tx(session_info.l1_block_number)
                .await?;

            if withdrawal_txid.is_some() {
                return Ok(false);
            }

            let unsigned_tx = UnsignedWithdrawalTx::from_bytes(&session_info.unsigned_tx);
            let signed_tx = self.sign_transaction(unsigned_tx.clone(), musig2_signature);

            let txid = self
                .btc_client
                .broadcast_signed_transaction(&signed_tx)
                .await?;

            self.master_connection_pool
                .connection_tagged("coordinator task")
                .await?
                .via_votes_dal()
                .mark_vote_transaction_as_processed_withdrawals(
                    H256::from_slice(&txid.as_raw_hash().to_byte_array()),
                    session_info.l1_block_number,
                )
                .await?;

            tracing::info!(
                "New withdrawal transaction processed, l1 batch {} musig2 tx_id {}",
                session_info.l1_block_number,
                txid
            );

            self.reinit_signer()?;

            return Ok(true);
        }
        Ok(false)
    }
}
