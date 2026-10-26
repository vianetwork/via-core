use zksync_db_connection::{
    advisory_lock::{AdvisoryLockKey, TransactionAdvisoryLock},
    connection::Connection,
    instrument::InstrumentExt,
};

use crate::{Core, CoreDal};

pub const BTC_WATCH_OWNER_LOCK_NAMESPACE: i32 = 1464095563;
const BTC_WATCH_OWNER_LOCK_RESOURCE: i32 = 0;
pub const BTC_WATCH_OWNER: TransactionAdvisoryLock = TransactionAdvisoryLock::new(
    AdvisoryLockKey::Pair(
        BTC_WATCH_OWNER_LOCK_NAMESPACE,
        BTC_WATCH_OWNER_LOCK_RESOURCE,
    ),
    "btc_watch_owner",
)
.report_latency();



#[derive(Debug)]
pub struct ViaIndexerDal<'a, 'c> {
    pub(crate) storage: &'a mut Connection<'c, Core>,
}

impl ViaIndexerDal<'_, '_> {
    /// Materialize only candidates selected through the freshly checked mature frontier.
    /// Actual L2 use remains behind the reader's global readiness and source gates.
    pub async fn apply_reader_protocol(
        &mut self,
        change: &via_btc_storage::StoredConfiguration,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        use via_btc_client::indexer::{configuration::ConfigurationInput, replay::compare_fields};
        use zksync_contracts::BaseSystemContractsHashes;
        use zksync_types::{
            protocol_upgrade::ProtocolVersion, protocol_version::L1VerifierConfig,
            via_protocol_upgrade::ViaProtocolUpgrade,
        };
        anyhow::ensure!(
            self.storage.in_transaction(),
            "protocol materialization outside transaction"
        );
        let ConfigurationInput::Protocol(_, proposals) = &change.change.payload.input else {
            anyhow::bail!("non-protocol candidate at native protocol owner");
        };
        let [proposal] = proposals.as_slice() else {
            anyhow::bail!("ambiguous protocol proposal at native protocol owner");
        };
        let input = &proposal.payload.input;
        let previous = via_btc_storage::ReaderDal::new(self.storage)
            .protocol_before(change.change.origin.height)
            .await?
            .context("previous canonical protocol is missing")?;
        let previous = self
            .storage
            .protocol_versions_dal()
            .get_protocol_version_with_latest_patch(previous.minor)
            .await?
            .context("previous native protocol is missing")?;
        let tx = ViaProtocolUpgrade::default()
            .create_protocol_upgrade_tx(input.version, input.system_contracts.clone())?;
        let tx_hash = tx.common_data.hash();
        let expected = ProtocolVersion {
            version: input.version,
            timestamp: 0,
            l1_verifier_config: L1VerifierConfig {
                snark_wrapper_vk_hash: input.recursion_scheduler_level_vk_hash,
            },
            base_system_contracts_hashes: BaseSystemContractsHashes {
                bootloader: input.bootloader_code_hash,
                default_aa: input.default_account_code_hash,
                evm_emulator: input
                    .evm_emulator_code_hash
                    .or(previous.base_system_contracts_hashes.evm_emulator),
            },
            tx: Some(tx),
        };
        let saved = self
            .storage
            .protocol_versions_dal()
            .get_protocol_version_with_latest_patch(input.version.minor)
            .await?;
        if let Some(saved) = &saved {
            compare_fields([
                (
                    "bootloader_code_hash",
                    saved.base_system_contracts_hashes.bootloader
                        == expected.base_system_contracts_hashes.bootloader,
                ),
                (
                    "default_account_code_hash",
                    saved.base_system_contracts_hashes.default_aa
                        == expected.base_system_contracts_hashes.default_aa,
                ),
                (
                    "evm_emulator_code_hash",
                    saved.base_system_contracts_hashes.evm_emulator
                        == expected.base_system_contracts_hashes.evm_emulator,
                ),
                (
                    "upgrade_tx_hash",
                    saved.tx.as_ref().map(|tx| tx.common_data.hash()) == Some(tx_hash),
                ),
            ])?;
            if let Some(patch) = self
                .storage
                .protocol_versions_dal()
                .l1_verifier_config_for_version(input.version)
                .await
            {
                compare_fields([(
                    "snark_wrapper_vk_hash",
                    patch.snark_wrapper_vk_hash == input.recursion_scheduler_level_vk_hash,
                )])?;
            }
            self.storage
                .protocol_versions_dal()
                .save_protocol_version(
                    expected.version,
                    expected.timestamp,
                    expected.l1_verifier_config,
                    expected.base_system_contracts_hashes,
                    Some(tx_hash),
                )
                .await?;
        } else {
            self.storage
                .protocol_versions_dal()
                .save_protocol_version_with_tx(&expected)
                .await?;
        }
        via_btc_storage::ReaderDal::new(self.storage)
            .bind_protocol(change)
            .await
    }

    // Era rollback's explicit child-before-parent deletion, limited to replayable Bitcoin rows.
    // Local L2, sender and DA parents remain in place.
    // https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/block_reverter/src/lib.rs
    pub async fn delete_reader_suffix(&mut self, height: u32) -> anyhow::Result<()> {
        self.lock_btc_watch().await?;
        via_btc_storage::ReaderDal::new(self.storage)
            .check_reset_schema(
                &[
                    "transactions",
                    "protocol_versions",
                    "protocol_patches",
                    "via_btc_proofs",
                    "via_btc_finalizations",
                    "via_votes",
                    "via_wallets",
                    "via_l1_blocks",
                    "via_btc_configuration",
                    "via_btc_headers",
                ],
                &[
                    "call_traces.call_traces_tx_hash_fkey",
                    "l1_batches.l1_batches_protocol_version_fkey",
                    "miniblocks.miniblocks_protocol_version_fkey",
                    "protocol_patches.protocol_patches_minor_fkey",
                    "via_btc_finalizations.via_btc_finalizations_proof_id_fkey",
                    "via_votes.via_votes_votable_transaction_id_fkey",
                    "via_btc_consumers.via_btc_consumers_header_id_fkey",
                    "via_btc_consumers.via_btc_consumers_oldest_needed_id_fkey",
                ],
            )
            .await?;
        via_btc_storage::ReaderDal::new(self.storage)
            .delete_common_suffix(height)
            .await?;
        for query in [
            "DELETE FROM via_btc_finalizations f USING via_btc_proofs p WHERE f.proof_id=p.id AND p.l1_block_number>$1",
            "DELETE FROM via_btc_proofs p WHERE p.l1_block_number>$1
                AND NOT EXISTS(SELECT 1 FROM via_btc_inscriptions_request r WHERE r.l1_batch_number=p.l1_batch_number)
                AND NOT EXISTS(SELECT 1 FROM via_data_availability d WHERE d.l1_batch_number=p.l1_batch_number)",
            "DELETE FROM protocol_patches p WHERE (p.bitcoin_height>$1 OR p.bitcoin_height IS NULL)
                AND NOT EXISTS(SELECT 1 FROM l1_batches b WHERE b.protocol_version=p.minor)
                AND NOT EXISTS(SELECT 1 FROM miniblocks b WHERE b.protocol_version=p.minor)
                AND NOT EXISTS(SELECT 1 FROM protocol_versions v JOIN transactions t ON t.hash=v.upgrade_tx_hash
                    WHERE v.id=p.minor AND (t.l1_batch_number IS NOT NULL OR t.miniblock_number IS NOT NULL))",
            "DELETE FROM transactions t WHERE t.is_priority AND COALESCE(t.bitcoin_height,t.l1_block_number)>$1
                AND t.l1_batch_number IS NULL AND t.miniblock_number IS NULL
                AND NOT EXISTS(SELECT 1 FROM call_traces c WHERE c.tx_hash=t.hash)",
        ] {
            sqlx::query(query).bind(i64::from(height)).instrument("main_bitcoin_delete_suffix")
                .execute(self.storage).await?;
        }
        Ok(())
    }


    /// Lets only one main-node Bitcoin scan, header update, initialization or recovery run at a time.
    /// Call it inside the transaction that commits the work, before reading progress, fetching blocks or taking other locks.
    /// The key is a two-integer pair with its own first number, so it collides neither with the verifier's
    /// bigint keys nor with its two-integer wallet-session keys.
    // Graph Node's `DeploymentStore::rewind_or_truncate_with_conn` saves indexed data and block pointer under one owner.
    // Via retains separate tasks, so they share this transaction owner through Bitcoin fetches.
    // https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs
    pub async fn lock_btc_watch(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.storage.in_transaction(),
            "Bitcoin watch ownership requires a transaction"
        );
        BTC_WATCH_OWNER
            .acquire(self.storage)
            .await
            .map_err(Into::into)
    }
}

impl via_btc_storage::ReaderRole for Core {
    const NAME: &'static str = "core";
    const OWNER: TransactionAdvisoryLock = BTC_WATCH_OWNER;
    const REQUIRED_CONSUMERS: &'static [&'static str] = &[via_btc_storage::PROTOCOL_CONSUMER];

    async fn authorize_publication(
        storage: &mut Connection<'_, Self>,
        _message: &via_btc_client::types::InscriptionMessage,
        voter: &bitcoin::Address,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            via_btc_storage::ReaderDal::new(storage)
                .mature_wallets()
                .await?
                .sequencer
                == *voter,
            via_btc_storage::StoreError::AuthorityHeld
        );
        Ok(())
    }
    async fn save_publication(
        storage: &mut Connection<'_, Self>,
        request_id: i64,
        p: via_btc_storage::publication::Publication,
    ) -> anyhow::Result<()> {
        use bitcoin::hashes::Hash;
        storage
            .btc_sender_dal()
            .insert_inscription_request_history(
                &p.commit_txid.to_byte_array(),
                &p.reveal_txid.to_byte_array(),
                request_id,
                &p.signed_commit,
                // jscpd:ignore-start ReaderRole trait signatures and role-owned SQL tails.
                &p.signed_reveal,
                p.fees,
                p.sent_at,
            )
            .await?;
        Ok(())
    }
    async fn save_proof(
        storage: &mut Connection<'_, Self>,
        proof: &via_btc_client::indexer::replay::Included<
            via_btc_client::indexer::replay::ProofInput,
        >,
        header_id: i64,
    ) -> anyhow::Result<()> {
        storage
            .via_votes_dal()
            .save_reader_proof(proof, header_id)
            .await
    }
    async fn initialize_bootstrap(
        storage: &mut Connection<'_, Self>,
        bootstrap: &via_btc_client::bootstrap::CheckedBootstrap,
    ) -> anyhow::Result<()> {
        // jscpd:ignore-end
        use anyhow::Context;
        let native = storage
            .protocol_versions_dal()
            .get_protocol_version_with_latest_patch(bootstrap.state.protocol_version.minor)
            .await?
            .context("native bootstrap protocol is missing")?;
        anyhow::ensure!(
            native.base_system_contracts_hashes.bootloader == bootstrap.state.bootloader_hash
                && native.base_system_contracts_hashes.default_aa
                    == bootstrap.state.abstract_account_hash
                && native
                    .base_system_contracts_hashes
                    .evm_emulator
                    .unwrap_or_default()
                    == bootstrap.state.evm_emulator_hash,
            "Bootstrap native contract hashes differ"
        );
        let verifier_matches = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM protocol_patches
            WHERE minor=$1 AND patch=$2 AND snark_wrapper_vk_hash=$3)",
        )
        .bind(bootstrap.state.protocol_version.minor as i32)
        .bind(i32::try_from(bootstrap.state.protocol_version.patch.0)?)
        .bind(bootstrap.state.snark_wrapper_vk_hash.as_bytes())
        .instrument("main_bootstrap_verifier_config")
        .fetch_one(storage)
        .await?;
        anyhow::ensure!(
            verifier_matches,
            "Bootstrap native verifier configuration differs"
        );
        Ok(())
    }

    async fn lock_source(_storage: &mut Connection<'_, Self>) -> anyhow::Result<()> {
        // jscpd:ignore-start ReaderRole trait signatures and role-owned SQL tails.
        Ok(())
    }
    async fn delete_suffix(storage: &mut Connection<'_, Self>, height: u32) -> anyhow::Result<()> {
        storage.via_indexer_dal().delete_reader_suffix(height).await
    }
    async fn materialize_protocol(
        storage: &mut Connection<'_, Self>,
        change: &via_btc_storage::StoredConfiguration,
    ) -> anyhow::Result<()> {
        storage
            .via_indexer_dal()
            // jscpd:ignore-end
            .apply_reader_protocol(change)
            .await
    }
    async fn apply_protocol_block(
        storage: &mut Connection<'_, Self>,
        block: &via_btc_storage::protocol::ProtocolBlock,
    ) -> anyhow::Result<()> {
        for deposit in &block.deposits {
            storage
                .via_transactions_dal()
                .insert_transaction_l1(
                    &deposit.operation,
                    deposit.operation.eth_block(),
                    deposit.txid,
                    &deposit.origin,
                    &deposit.payload,
                )
                .await?;
        }
        Ok(())
    }
    async fn stamp_legacy_cutoff(
        storage: &mut Connection<'_, Self>,
        cutoff: u32,
    ) -> anyhow::Result<()> {
        // jscpd:ignore-start ReaderRole trait signatures and role-owned SQL tails.
        sqlx::query(
            "UPDATE via_l1_batch_inscription_request SET rotation_legacy_cutoff=$1
    WHERE is_finalized IS NOT NULL AND rotation_legacy_cutoff IS NULL",
        )
        .bind(i64::from(cutoff))
        .instrument("bitcoin_legacy_rotation_cutoff")
        .execute(storage)
        .await?;
        Ok(())
    }
    // jscpd:ignore-end

    // jscpd:ignore-start ReaderRole trait signatures and role-owned SQL tails.
    async fn unreconciled_source(
        storage: &mut Connection<'_, Self>,
    ) -> anyhow::Result<Option<String>> {
        sqlx::query_scalar::<_, String>("(SELECT 'deposit:' || encode(t.hash,'hex') FROM transactions t
            WHERE t.is_priority AND t.upgrade_id IS NULL AND (t.bitcoin_payload IS NULL
                OR NOT EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.number=t.bitcoin_height AND h.hash=t.bitcoin_hash)) LIMIT 1)
            UNION ALL (SELECT 'protocol_minor:' || v.id::TEXT FROM protocol_versions v WHERE (
                EXISTS(SELECT 1 FROM l1_batches b WHERE b.protocol_version=v.id)
                OR EXISTS(SELECT 1 FROM miniblocks b WHERE b.protocol_version=v.id))
                AND NOT EXISTS(SELECT 1 FROM protocol_patches p JOIN via_btc_headers h
                    ON h.number=p.bitcoin_height AND h.hash=p.bitcoin_hash WHERE p.minor=v.id) LIMIT 1) LIMIT 1")
            .instrument("bitcoin_reader_retained_reconciliation").fetch_optional(storage).await.map_err(Into::into)
    }
    // jscpd:ignore-end

    // jscpd:ignore-start ReaderRole trait signatures and role-owned SQL tails.
    async fn retained_boundary(storage: &mut Connection<'_, Self>) -> anyhow::Result<Option<u32>> {
        let height = sqlx::query_scalar::<_, Option<i64>>("SELECT max(height) FROM (
            SELECT h.number AS height FROM transactions t JOIN via_btc_headers h ON h.number=t.bitcoin_height AND h.hash=t.bitcoin_hash
                WHERE t.is_priority AND (t.l1_batch_number IS NOT NULL OR t.miniblock_number IS NOT NULL)
            UNION ALL SELECT h.number FROM protocol_patches p JOIN via_btc_headers h ON h.number=p.bitcoin_height AND h.hash=p.bitcoin_hash
                WHERE EXISTS(SELECT 1 FROM l1_batches b WHERE b.protocol_version=p.minor)
                   OR EXISTS(SELECT 1 FROM miniblocks b WHERE b.protocol_version=p.minor)
                   OR EXISTS(SELECT 1 FROM protocol_versions v JOIN transactions t ON t.hash=v.upgrade_tx_hash
                       WHERE v.id=p.minor AND (t.l1_batch_number IS NOT NULL OR t.miniblock_number IS NOT NULL))
            UNION ALL SELECT h.number FROM via_btc_proofs p JOIN via_btc_headers h ON h.id=p.proof_header_id
                JOIN via_l1_batch_inscription_request r ON r.l1_batch_number=p.l1_batch_number
                JOIN via_btc_inscriptions_request i ON i.id=r.commit_proof_inscription_id
                JOIN via_btc_inscriptions_request_history sent ON sent.id=i.confirmed_inscriptions_request_history_id
                WHERE r.rotation_legacy_cutoff IS NOT NULL AND sent.reveal_tx_id=p.proof_reveal_tx_id
            UNION ALL SELECT h.number FROM via_votes v JOIN via_btc_headers h ON h.number=v.source_l1_block_number AND h.hash=v.source_l1_block_hash
                JOIN via_btc_proofs p ON p.id=v.votable_transaction_id
                JOIN via_l1_batch_inscription_request r ON r.l1_batch_number=p.l1_batch_number
                JOIN via_btc_inscriptions_request i ON i.id=r.commit_proof_inscription_id
                JOIN via_btc_inscriptions_request_history sent ON sent.id=i.confirmed_inscriptions_request_history_id
                WHERE h.number<=r.rotation_legacy_cutoff AND sent.reveal_tx_id=p.proof_reveal_tx_id
            ) retained")
            .instrument("bitcoin_retained_authority_boundary").fetch_one(storage).await?;
        height.map(u32::try_from).transpose().map_err(Into::into)
    }

    async fn retained_proof_txids(
        storage: &mut Connection<'_, Self>,
    ) -> anyhow::Result<Vec<bitcoin::Txid>> {
        // jscpd:ignore-end
        use bitcoin::hashes::Hash;
        let rows = sqlx::query_as::<_, (Vec<u8>,)>("SELECT DISTINCT sent.reveal_tx_id FROM via_l1_batch_inscription_request r
    JOIN via_btc_inscriptions_request i ON i.id=r.commit_proof_inscription_id
    JOIN via_btc_inscriptions_request_history sent ON sent.id=i.confirmed_inscriptions_request_history_id
    WHERE NOT EXISTS(SELECT 1 FROM via_btc_proofs p JOIN via_btc_headers h ON h.id=p.proof_header_id
        AND h.hash=p.block_hash WHERE p.proof_reveal_tx_id=sent.reveal_tx_id)")
            .instrument("bitcoin_retained_proof_sources").fetch_all(storage).await?;
        // jscpd:ignore-start ReaderRole trait signatures and role-owned SQL tails.
        rows.into_iter()
            .map(|(bytes,)| bitcoin::Txid::from_slice(&bytes).map_err(Into::into))
            .collect()
    }

    async fn canonical_proofs(
        storage: &mut Connection<'_, Self>,
        through: u32,
        scope: via_btc_storage::ProofScope,
    ) -> anyhow::Result<Vec<via_btc_storage::CanonicalProof>> {
        let txid = match scope {
            via_btc_storage::ProofScope::Publication(txid) => Some({
                // jscpd:ignore-end
                use bitcoin::hashes::Hash;
                txid.to_byte_array().to_vec()
            }),
            _ => None,
        };
        let rows = sqlx::query("SELECT p.id,p.payload,h.id AS header_id,h.number AS height,h.hash, TRUE AS local_complete, EXISTS(SELECT 1 FROM via_l1_batch_inscription_request r
               JOIN via_btc_inscriptions_request i ON i.id=r.commit_proof_inscription_id
               JOIN via_btc_inscriptions_request_history sent ON sent.id=i.confirmed_inscriptions_request_history_id
               WHERE r.l1_batch_number=p.l1_batch_number AND r.rotation_legacy_cutoff>=h.number
                 AND sent.reveal_tx_id=p.proof_reveal_tx_id) AS rotation_exempt
             FROM via_btc_proofs p JOIN via_btc_headers h ON h.id=p.proof_header_id AND h.number=p.l1_block_number AND h.hash=p.block_hash
             WHERE h.number<=$1 AND ($3::BYTEA IS NULL OR p.proof_reveal_tx_id=$3) AND (NOT $2 OR (
                NOT EXISTS(SELECT 1 FROM via_btc_finalizations f WHERE f.proof_id=p.id)
                AND EXISTS(SELECT 1 FROM via_l1_batch_inscription_request r JOIN via_btc_inscriptions_request i
                    ON i.id=r.commit_proof_inscription_id JOIN via_btc_inscriptions_request_history sent
                    ON sent.id=i.confirmed_inscriptions_request_history_id
                    WHERE r.l1_batch_number=p.l1_batch_number AND sent.reveal_tx_id=p.proof_reveal_tx_id))) ORDER BY h.number,p.transaction_index,p.message_index").bind(i64::from(through)).bind(matches!(scope, via_btc_storage::ProofScope::Finalization)).bind(txid)
            // jscpd:ignore-start ReaderRole trait signatures and role-owned SQL tails.
            .map(|row: sqlx::postgres::PgRow| row).instrument("bitcoin_canonical_proofs").fetch_all(storage).await?;
        rows.iter()
            .map(via_btc_storage::CanonicalProof::from_row)
            .collect()
    }

    async fn finalize_proof(
        storage: &mut Connection<'_, Self>,
        work: &via_btc_storage::ProofWork,
        approved: bool,
    ) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            // jscpd:ignore-end
            "INSERT INTO via_btc_finalizations(proof_id,approved)
    SELECT p.id,$3 FROM via_btc_proofs p JOIN via_btc_headers h
      ON h.id=p.proof_header_id AND h.number=p.l1_block_number AND h.hash=p.block_hash
    WHERE p.id=$1 AND p.proof_header_id=$2 ON CONFLICT(proof_id) DO NOTHING",
        )
        .bind(work.id)
        .bind(work.header_id)
        .bind(approved)
        .instrument("bitcoin_finalize_exact_proof")
        .execute(storage)
        .await?
        .rows_affected())
    }
}
