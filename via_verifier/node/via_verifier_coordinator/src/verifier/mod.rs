use crate::{
    auth::{digest, random_id, Binding, Envelope, MAX_BODY},
    sessions::withdrawal::WithdrawalSession,
    traits::ISession,
    types::{NoncePair, PartialSignaturePair, SessionOperation, SigningSessionResponse},
    utils::{decode_nonce, decode_signature, encode_nonce, encode_signature},
};
use anyhow::Context;
use bitcoin::{
    secp256k1::{PublicKey, Secp256k1, SecretKey},
    TapSighashType, Transaction, Witness,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, str::FromStr, sync::Arc};
use tokio::sync::watch;
use via_btc_client::traits::BitcoinOps;
use via_musig2::{
    get_signer_with_merkle_root,
    transaction_builder::TransactionBuilder,
    utils::{aggregate_public_signatures, verify_partial_signature},
    Signer,
};
use via_verifier_dal::{
    withdrawals_dal::{WithdrawalAttemptRecord, WithdrawalAttemptState},
    Connection, ConnectionPool, Verifier, VerifierDal,
};
use via_verifier_state::sync::ViaState;
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::{
    configs::{
        via_bridge::ViaBridgeConfig, via_verifier::ViaVerifierConfig, via_wallets::ViaWallet,
    },
    ViaBtcWatchConfig,
};
use zksync_types::{via_roles::ViaNodeRole, via_wallet::SystemWallets};

type PublicTranscript = BTreeMap<usize, BTreeMap<usize, String>>;
#[derive(Serialize, Deserialize)]
pub(crate) struct CachedShares {
    pub nonces: PublicTranscript,
    pub shares: BTreeMap<usize, PartialSignaturePair>,
}
use crate::types::AdmittedContent;
struct LiveRound {
    id: [u8; 32],
    content: Vec<u8>,
    signers: Option<Vec<Signer>>,
    // Keep the exact batch with its secrets even if COMMIT returns an uncertain result.
    public_nonces: Vec<u8>,
}

pub struct ViaWithdrawalVerifier {
    verifier_config: ViaVerifierConfig,
    wallet: ViaWallet,
    withdrawal_session: WithdrawalSession,
    btc_client: Arc<dyn BitcoinOps>,
    master_connection_pool: ConnectionPool<Verifier>,
    client: reqwest::Client,
    via_bridge_config: ViaBridgeConfig,
    state: ViaState,
    live: Option<LiveRound>,
}

impl ViaWithdrawalVerifier {
    pub fn new(
        verifier_config: ViaVerifierConfig,
        wallet: ViaWallet,
        master_connection_pool: ConnectionPool<Verifier>,
        btc_client: Arc<dyn BitcoinOps>,
        withdrawal_client: WithdrawalClient,
        via_bridge_config: ViaBridgeConfig,
        via_btc_watch_config: ViaBtcWatchConfig,
    ) -> anyhow::Result<Self> {
        if verifier_config.withdrawal_signing_enabled {
            PublicKey::from_str(&verifier_config.coordinator_public_key)?;
            anyhow::ensure!(
                via_bridge_config
                    .verifiers_pub_keys
                    .contains(&verifier_config.coordinator_public_key),
                "Coordinator must belong to wallet"
            );
            let unique: std::collections::BTreeSet<_> =
                via_bridge_config.verifiers_pub_keys.iter().collect();
            anyhow::ensure!(
                unique.len() == via_bridge_config.verifiers_pub_keys.len(),
                "Duplicate participants"
            );
        }
        let withdrawal_session = WithdrawalSession::new(
            master_connection_pool.clone(),
            Arc::new(TransactionBuilder::new(btc_client.clone())?),
            withdrawal_client,
        );
        let state = ViaState::new(
            master_connection_pool.clone(),
            btc_client.clone(),
            via_btc_watch_config,
        );
        Ok(Self {
            verifier_config,
            wallet,
            withdrawal_session,
            btc_client,
            master_connection_pool,
            client: reqwest::Client::new(),
            via_bridge_config,
            state,
            live: None,
        })
    }

    pub async fn run(mut self, mut stop: watch::Receiver<bool>) -> anyhow::Result<()> {
        if !self.verifier_config.withdrawal_signing_enabled {
            while !*stop.borrow_and_update() {
                if stop.changed().await.is_err() {
                    break;
                }
            }
            return Ok(());
        }
        self.withdrawal_session.prepare_session().await?;
        let wallet = self.withdrawal_session.wallet_script().await?;
        let pool = self.master_connection_pool.clone();
        let mut owner = pool.connection().await?;
        anyhow::ensure!(
            owner
                .via_withdrawal_dal()
                .acquire_withdrawal_signer_lock(&wallet)
                .await?,
            "Another signer process owns this wallet"
        );
        owner
            .via_withdrawal_dal()
            .retire_incomplete_withdrawal_attempts(&wallet)
            .await?;
        let mut timer = tokio::time::interval(self.verifier_config.polling_interval());
        while !*stop.borrow_and_update() {
            tokio::select! { _ = timer.tick() => {}, _ = stop.changed() => break }
            // Never replace/reacquire this connection: a lost owner terminates the process task.
            Self::check_owner(&mut owner, &wallet).await?;
            if let Err(error) = self.iteration(&mut owner, &wallet).await {
                crate::metrics::METRICS.errors.inc();
                tracing::error!("Withdrawal signing deferred: {error:#}");
            }
        }
        self.live = None;
        owner
            .via_withdrawal_dal()
            .release_withdrawal_signer_lock(&wallet)
            .await?;
        Ok(())
    }

    async fn check_owner(
        owner: &mut Connection<'_, Verifier>,
        wallet: &[u8],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            owner
                .via_withdrawal_dal()
                .check_withdrawal_signer_lock(wallet)
                .await?,
            "Signer ownership lost"
        );
        Ok(())
    }

    fn secret(&self) -> anyhow::Result<SecretKey> {
        Ok(bitcoin::PrivateKey::from_wif(&self.wallet.private_key)?.inner)
    }
    fn signer_index(&self) -> anyhow::Result<usize> {
        let public = PublicKey::from_secret_key(&Secp256k1::new(), &self.secret()?).to_string();
        self.via_bridge_config
            .verifiers_pub_keys
            .iter()
            .position(|key| key == &public)
            .context("Local signer not in wallet")
    }

    async fn request(
        &self,
        method: &str,
        target: &str,
        round: [u8; 32],
        content: [u8; 32],
        body: Vec<u8>,
    ) -> anyhow::Result<Vec<u8>> {
        let key = self.secret()?;
        let binding = Binding {
            version: 1,
            sequencer_version: via_verifier_types::protocol_version::get_sequencer_version()
                .to_string(),
            principal: PublicKey::from_secret_key(&Secp256k1::new(), &key).to_string(),
            audience: self.verifier_config.coordinator_public_key.clone(),
            method: method.into(),
            target: target.into(),
            round,
            content,
            challenge: random_id(),
            timestamp: chrono::Utc::now().timestamp(),
            status: 0,
        };
        let envelope = Envelope::sign(binding.clone(), body, &key)?;
        let mut response = self
            .client
            .request(
                reqwest::Method::from_bytes(method.as_bytes())?,
                format!(
                    "{}{}",
                    self.verifier_config
                        .coordinator_http_url
                        .trim_end_matches('/'),
                    target
                ),
            )
            .timeout(std::time::Duration::from_secs(u64::from(
                self.verifier_config.verifier_request_timeout,
            )))
            .json(&envelope)
            .send()
            .await?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                bytes.len() + chunk.len() <= MAX_BODY,
                "Oversized coordinator response"
            );
            bytes.extend_from_slice(&chunk);
        }
        let reply: Envelope = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "Invalid authenticated coordinator response for {method} {target}: HTTP {status}"
            )
        })?;
        reply.verify_response(
            &binding,
            &PublicKey::from_str(&self.verifier_config.coordinator_public_key)?,
            status.as_u16(),
            chrono::Utc::now().timestamp(),
            self.verifier_config.verifier_request_timeout,
        )?;
        anyhow::ensure!(
            status.is_success(),
            "Coordinator rejected request: {}",
            String::from_utf8_lossy(&reply.body)
        );
        Ok(reply.body)
    }

    async fn snapshot(&self) -> anyhow::Result<SigningSessionResponse> {
        Ok(serde_json::from_slice(
            &self
                .request("GET", "/session/", [0; 32], [0; 32], Vec::new())
                .await?,
        )?)
    }

    async fn iteration(
        &mut self,
        owner: &mut Connection<'_, Verifier>,
        wallet: &[u8],
    ) -> anyhow::Result<()> {
        self.withdrawal_session.prepare_session().await?;
        if self.state.is_reorg_in_progress().await? || self.state.is_sync_in_progress().await? {
            return Ok(());
        }
        self.validate_verifier_addresses().await?;
        anyhow::ensure!(
            self.withdrawal_session.wallet_script().await? == wallet,
            "Wallet changed while signer active"
        );
        for record in owner
            .via_withdrawal_dal()
            .list_recoverable_withdrawal_attempts(wallet)
            .await?
        {
            if let Some(bytes) = &record.finalized_transaction {
                let recovered: anyhow::Result<()> = async {
                    let bound: AdmittedContent = bincode::deserialize(&record.content)?;
                    anyhow::ensure!(
                        bound.wallet == wallet
                            && bound.network == self.btc_client.get_network()
                            && bound.chain_id == self.withdrawal_session.chain_id().await?
                            && bound.participants == self.via_bridge_config.verifiers_pub_keys
                            && bound.tweak == self.verifier_config.bridge_address_merkle_root,
                        "Changed finalized recovery domain"
                    );
                    let operation: SessionOperation = bincode::deserialize(&bound.proposal)?;
                    self.broadcast(owner, wallet, &operation, bytes).await
                }
                .await;
                if let Err(error) = recovered {
                    Self::check_owner(owner, wallet).await?;
                    crate::metrics::METRICS.errors.inc();
                    tracing::error!(round = %hex::encode(&record.round_id), "Withdrawal recovery deferred: {error:#}");
                }
            }
        }
        let mut snapshot = self.snapshot().await?;
        if self.verifier_config.role == ViaNodeRole::Coordinator {
            self.request(
                "POST",
                "/session/new",
                snapshot.round_id,
                snapshot.content_hash,
                Vec::new(),
            )
            .await?;
            snapshot = self.snapshot().await?;
        }
        if self
            .live
            .as_ref()
            .is_some_and(|live| live.id != snapshot.round_id)
        {
            let old = self.live.take().unwrap();
            let record = owner
                .via_withdrawal_dal()
                .get_withdrawal_attempt(wallet, &old.id)
                .await?;
            if record.as_ref().is_some_and(|record| {
                !matches!(
                    record.state,
                    WithdrawalAttemptState::Signed | WithdrawalAttemptState::Finalized
                )
            }) {
                owner
                    .via_withdrawal_dal()
                    .retire_withdrawal_attempt(wallet, &old.id)
                    .await?;
            }
        }
        if snapshot.session_op.is_empty() {
            return Ok(());
        }
        anyhow::ensure!(
            snapshot.round_id != [0; 32]
                && digest(&snapshot.authorized_content) == snapshot.content_hash,
            "Invalid proposal commitment"
        );
        anyhow::ensure!(
            snapshot.required_signers == self.via_bridge_config.verifiers_pub_keys.len(),
            "Changed participant count"
        );
        let operation: SessionOperation = bincode::deserialize(&snapshot.session_op)?;
        let messages = operation.get_message_to_sign();
        anyhow::ensure!(!messages.is_empty(), "Empty signing round");
        validate_transcript(&snapshot.nonces, messages.len(), snapshot.required_signers)?;
        validate_transcript(
            &snapshot.signatures,
            messages.len(),
            snapshot.required_signers,
        )?;
        let existing = owner
            .via_withdrawal_dal()
            .get_withdrawal_attempt(wallet, &snapshot.round_id)
            .await?;
        let record = if let Some(record) = existing {
            let bound: AdmittedContent = bincode::deserialize(&record.content)?;
            anyhow::ensure!(
                record.content == snapshot.authorized_content
                    && bound.proposal == snapshot.session_op
                    && bound.wallet == wallet
                    && bound.chain_id == self.withdrawal_session.chain_id().await?
                    && bound.network == self.btc_client.get_network()
                    && bound.participants == self.via_bridge_config.verifiers_pub_keys
                    && bound.tweak == self.verifier_config.bridge_address_merkle_root,
                "Changed admitted context"
            );
            record
        } else {
            let expected = self
                .withdrawal_session
                .authorize_withdrawal(&operation)
                .await?;
            let content = bincode::serialize(&AdmittedContent {
                proposal: snapshot.session_op.clone(),
                network: self.btc_client.get_network(),
                chain_id: self.withdrawal_session.chain_id().await?,
                wallet: wallet.to_vec(),
                participants: self.via_bridge_config.verifiers_pub_keys.clone(),
                tweak: self.verifier_config.bridge_address_merkle_root.clone(),
                expected: expected.clone(),
            })?;
            anyhow::ensure!(
                content == snapshot.authorized_content,
                "Coordinator expected snapshot or wallet domain differs"
            );
            owner
                .via_withdrawal_dal()
                .admit_withdrawal_attempt(
                    wallet,
                    &snapshot.round_id,
                    &content,
                    &expected,
                    &operation.get_unsigned_bridge_tx().utxos,
                )
                .await?
        };
        if record.state == WithdrawalAttemptState::Retired {
            anyhow::bail!("Retired round cannot recreate nonce");
        }
        if let Some(bytes) = &record.finalized_transaction {
            return self.broadcast(owner, wallet, &operation, bytes).await;
        }
        if let Some(bytes) = &record.public_signatures {
            let cached: CachedShares = serde_json::from_slice(bytes)?;
            return self
                .publish_cached(owner, wallet, &snapshot, &operation, &record, &cached)
                .await;
        }
        if record.state == WithdrawalAttemptState::MayHaveSigned {
            anyhow::bail!("Uncertain round held without nonce recreation");
        }
        if self.live.is_none() {
            if record.public_nonces.is_some() {
                owner
                    .via_withdrawal_dal()
                    .retire_withdrawal_attempt(wallet, &snapshot.round_id)
                    .await?;
                anyhow::bail!("Nonce secret unavailable; round retired");
            }
            let mut signers = Vec::with_capacity(messages.len());
            let mut nonces = BTreeMap::new();
            for (input, message) in messages.iter().enumerate() {
                let mut signer = get_signer_with_merkle_root(
                    &self.wallet.private_key,
                    self.via_bridge_config.verifiers_pub_keys.clone(),
                    self.verifier_config.bridge_address_merkle_root(),
                )?;
                let nonce = signer.start_signing_session(message.clone())?;
                nonces.insert(input, encode_nonce(signer.signer_index(), nonce)?);
                signers.push(signer);
            }
            let bytes = serde_json::to_vec(&nonces)?;
            self.live = Some(LiveRound {
                id: snapshot.round_id,
                content: record.content.clone(),
                signers: Some(signers),
                public_nonces: bytes,
            });
        }
        let live = self.live.as_ref().unwrap();
        anyhow::ensure!(
            live.id == snapshot.round_id && live.content == record.content,
            "Mixed live signing round"
        );
        // A failed write must retry the same nonce, never generate a replacement. If
        // COMMIT succeeded but its acknowledgement was lost, readback confirms the
        // identical batch; if it rolled back, the idempotent write commits it now.
        if let Some(persisted) = &record.public_nonces {
            anyhow::ensure!(
                persisted == &live.public_nonces,
                "Durable nonce differs from live secret"
            );
        } else {
            owner
                .via_withdrawal_dal()
                .persist_withdrawal_nonces(wallet, &live.id, &live.content, &live.public_nonces)
                .await?;
        }
        let nonce_bytes = &live.public_nonces;
        let own_nonces: BTreeMap<usize, NoncePair> = serde_json::from_slice(nonce_bytes)?;
        let index = self.signer_index()?;
        if !contains_batch(&snapshot.nonces, &own_nonces, index)? {
            Self::check_owner(owner, wallet).await?;
            self.request(
                "POST",
                "/session/nonce",
                snapshot.round_id,
                snapshot.content_hash,
                nonce_bytes.clone(),
            )
            .await?;
            return Ok(());
        }
        if !complete(&snapshot.nonces, messages.len(), snapshot.required_signers) {
            return Ok(());
        }
        // Decode and receive the full transcript before making signing risk durable.
        // Invalid public data must not strand an otherwise unsigned reservation.
        let signers = self
            .live
            .as_mut()
            .unwrap()
            .signers
            .as_mut()
            .context("Round nonce already consumed")?;
        for (input, signer) in signers.iter_mut().enumerate() {
            for (other, nonce) in &snapshot.nonces[&input] {
                if *other != index {
                    signer.receive_nonce(
                        *other,
                        decode_nonce(NoncePair {
                            signer_index: *other,
                            nonce: nonce.clone(),
                        })?,
                    )?;
                }
            }
        }
        // BIP327 nonce consumption is irreversible. The early durable marker deliberately
        // strands uncertain attempts rather than releasing requests after a crash.
        // https://github.com/bitcoin/bips/blob/eba8e50cb66d436c65c6bc8b0a175b643effe9d3/bip-0327.mediawiki#nonce-generation
        owner
            .via_withdrawal_dal()
            .mark_withdrawal_may_have_signed(wallet, &snapshot.round_id, &record.content)
            .await?;
        let mut signers = self
            .live
            .as_mut()
            .unwrap()
            .signers
            .take()
            .context("Round nonce already consumed")?;
        let mut shares = BTreeMap::new();
        for (input, signer) in signers.iter_mut().enumerate() {
            shares.insert(
                input,
                encode_signature(index, signer.create_partial_signature()?)?,
            );
        }
        let cached = CachedShares {
            nonces: snapshot.nonces.clone(),
            shares,
        };
        let bytes = serde_json::to_vec(&cached)?;
        owner
            .via_withdrawal_dal()
            .persist_withdrawal_signatures(wallet, &snapshot.round_id, &record.content, &bytes)
            .await?;
        self.publish_cached(owner, wallet, &snapshot, &operation, &record, &cached)
            .await
    }

    async fn publish_cached(
        &self,
        owner: &mut Connection<'_, Verifier>,
        wallet: &[u8],
        snapshot: &SigningSessionResponse,
        operation: &SessionOperation,
        record: &WithdrawalAttemptRecord,
        cached: &CachedShares,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            cached.nonces == snapshot.nonces,
            "Coordinator changed signed nonce transcript"
        );
        let index = self.signer_index()?;
        let mut submitted = true;
        for (input, pair) in &cached.shares {
            match snapshot.signatures.get(input).and_then(|m| m.get(&index)) {
                Some(value) => {
                    anyhow::ensure!(value == &pair.signature, "Coordinator changed cached share")
                }
                None => submitted = false,
            }
        }
        if !submitted {
            Self::check_owner(owner, wallet).await?;
            self.request(
                "POST",
                "/session/signature",
                snapshot.round_id,
                snapshot.content_hash,
                serde_json::to_vec(&cached.shares)?,
            )
            .await?;
            return Ok(());
        }
        let messages = operation.get_message_to_sign();
        if !complete(
            &snapshot.signatures,
            messages.len(),
            snapshot.required_signers,
        ) {
            return Ok(());
        }
        let mut transaction = operation.get_unsigned_bridge_tx().tx;
        for (input, message) in messages.iter().enumerate() {
            let nonces = snapshot.nonces[&input]
                .iter()
                .map(|(index, nonce)| {
                    decode_nonce(NoncePair {
                        signer_index: *index,
                        nonce: nonce.clone(),
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let signatures = snapshot.signatures[&input]
                .values()
                .map(|sig| decode_signature(sig.clone()))
                .collect::<anyhow::Result<Vec<_>>>()?;
            for (signer, signature) in signatures.iter().enumerate() {
                if let Err(error) = verify_partial_signature(
                    nonces[signer].clone(),
                    nonces.clone(),
                    self.via_bridge_config.verifiers_pub_keys[signer].clone(),
                    self.via_bridge_config.verifiers_pub_keys.clone(),
                    *signature,
                    message,
                    self.verifier_config.bridge_address_merkle_root(),
                ) {
                    crate::metrics::METRICS.verifier_errors[&crate::metrics::VerifierErrorLabel {
                        pubkey: self.via_bridge_config.verifiers_pub_keys[signer].clone(),
                        kind: crate::metrics::ErrorKind::PartialSignature,
                    }]
                        .inc();
                    return Err(error);
                }
            }
            let final_signature = aggregate_public_signatures(
                self.via_bridge_config.verifiers_pub_keys.clone(),
                self.verifier_config.bridge_address_merkle_root(),
                nonces,
                signatures,
                message,
            )?;
            let mut witness = final_signature.serialize().to_vec();
            witness.push(TapSighashType::All as u8);
            transaction.input[input].witness = Witness::from(vec![witness]);
        }
        let bytes = bitcoin::consensus::serialize(&transaction);
        owner
            .via_withdrawal_dal()
            .finalize_withdrawal_attempt(wallet, &snapshot.round_id, &record.content, &bytes)
            .await?;
        crate::metrics::METRICS
            .session_time
            .set(crate::utils::seconds_since_epoch().saturating_sub(snapshot.created_at) as usize);
        self.broadcast(owner, wallet, operation, &bytes).await
    }

    async fn broadcast(
        &self,
        owner: &mut Connection<'_, Verifier>,
        wallet: &[u8],
        operation: &SessionOperation,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        if !self
            .withdrawal_session
            .before_broadcast_final_transaction(operation)
            .await?
        {
            return Ok(());
        }
        Self::check_owner(owner, wallet).await?;
        let transaction: Transaction = bitcoin::consensus::deserialize(bytes)?;
        let authorized = operation.get_unsigned_bridge_tx().tx;
        anyhow::ensure!(
            transaction.version == authorized.version
                && transaction.lock_time == authorized.lock_time
                && transaction.output == authorized.output
                && transaction.input.len() == authorized.input.len()
                && transaction
                    .input
                    .iter()
                    .zip(&authorized.input)
                    .all(|(actual, expected)| {
                        actual.previous_output == expected.previous_output
                            && actual.sequence == expected.sequence
                            && actual.script_sig == expected.script_sig
                    }),
            "Finalized transaction differs from authorized proposal"
        );
        self.btc_client
            .broadcast_signed_transaction(&hex::encode(bytes))
            .await?;
        self.withdrawal_session
            .after_broadcast_final_transaction(&transaction, operation)
            .await?;
        Ok(())
    }

    async fn validate_verifier_addresses(&self) -> anyhow::Result<()> {
        let mut storage = self.master_connection_pool.connection().await?;
        let height = storage
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await?;
        let map = storage
            .via_wallet_dal()
            .get_system_wallets_raw(height as i64)
            .await?
            .context("System wallets not found")?;
        let wallets = SystemWallets::try_from(map)?;
        wallets.is_valid_verifier_address(self.verifier_config.wallet_address()?)?;
        wallets.is_valid_bridge_address(self.via_bridge_config.bridge_address()?)
    }
}

fn validate_transcript(
    transcript: &PublicTranscript,
    inputs: usize,
    signers: usize,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        transcript.keys().all(|index| *index < inputs)
            && transcript
                .values()
                .all(|values| values.keys().all(|index| *index < signers)),
        "Out-of-range transcript contribution"
    );
    for signer in 0..signers {
        let count = transcript
            .values()
            .filter(|values| values.contains_key(&signer))
            .count();
        anyhow::ensure!(count == 0 || count == inputs, "Partial participant batch");
    }
    Ok(())
}
fn complete(transcript: &PublicTranscript, inputs: usize, signers: usize) -> bool {
    transcript.len() == inputs
        && transcript.keys().copied().eq(0..inputs)
        && transcript
            .values()
            .all(|values| values.len() == signers && values.keys().copied().eq(0..signers))
}
fn contains_batch(
    transcript: &PublicTranscript,
    own: &BTreeMap<usize, NoncePair>,
    signer: usize,
) -> anyhow::Result<bool> {
    let mut present = true;
    for (input, pair) in own {
        match transcript.get(input).and_then(|values| values.get(&signer)) {
            Some(value) => {
                anyhow::ensure!(value == &pair.nonce, "Coordinator substituted local nonce")
            }
            None => present = false,
        }
    }
    Ok(present)
}

#[cfg(test)]
mod tests;
