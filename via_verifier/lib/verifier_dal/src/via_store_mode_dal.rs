//! Fixes a verifier database to one Bitcoin chain and one proof-verification mode.
//! A strict process must never adopt a store holding unproven development approvals.

use anyhow::{bail, ensure, Context};
use sqlx::Row;
use zksync_db_connection::{connection::Connection, instrument::InstrumentExt};

use crate::Verifier;

pub struct ViaStoreModeDal<'c, 'a> {
    pub(crate) storage: &'c mut Connection<'a, Verifier>,
}

/// The chain and proof-verification mode a verifier process runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreMode {
    pub proof_verification_dev_mode: bool,
    pub bitcoin_network: String,
    /// Genesis block hash of the connected Bitcoin node, which identifies the chain.
    pub bitcoin_genesis_hash: String,
}

impl ViaStoreModeDal<'_, '_> {
    /// Admits a process only when its mode matches the store's permanent designation, recorded on first start.
    /// Development results must never be read as cryptographic verdicts by a strict process.
    ///
    /// The chain is identified by its genesis hash, because a network name can silently default to regtest.
    /// Geth refuses a database whose stored genesis differs from the configured one.
    /// Via adapts that: storage init matches the node's genesis to the configured network, and the designation stores its hash:
    /// https://github.com/ethereum/go-ethereum/blob/920c07774c65ebb3023536f85df642c44478b540/core/genesis.go#L384-L392
    ///
    /// Returns true when a strict designation just adopted verdicts written before it, which stay unproven legacy results.
    /// A strict store keeps them, because re-verifying history needs genuine historical proofs.
    pub async fn ensure_proof_verification_mode(
        &mut self,
        mode: &StoreMode,
    ) -> anyhow::Result<bool> {
        ensure!(
            !mode.proof_verification_dev_mode || mode.bitcoin_network == "regtest",
            "proof verification development mode requires regtest, got {}",
            mode.bitcoin_network
        );
        let mut tx = self.storage.start_transaction().await?;
        let mut adopted_legacy = false;
        // The primary key serializes concurrent first starts, so only the process that inserts judges existing verdicts.
        let designated_now = sqlx::query(
            "INSERT INTO via_verifier_store_mode \
             (id, proof_verification_dev_mode, bitcoin_network, bitcoin_genesis_hash, designated_after_votable_id) \
             SELECT 1, $1, $2, $3, COALESCE(MAX(id), 0) FROM via_votable_transactions \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(mode.proof_verification_dev_mode)
        .bind(&mode.bitcoin_network)
        .bind(&mode.bitcoin_genesis_hash)
        .instrument("designate_verifier_store_mode")
        .execute(&mut tx)
        .await?
        .rows_affected()
            == 1;
        if designated_now {
            let has_verdicts: bool = sqlx::query(
                "SELECT EXISTS(SELECT 1 FROM via_votable_transactions WHERE l1_batch_status IS NOT NULL) AS found",
            )
            .instrument("store_mode_existing_verdicts")
            .fetch_optional(&mut tx)
            .await?
            .context("existence query returned no row")?
            .try_get("found")?;
            // Existing verdicts may come from the old skip path, and a development store is disposable.
            // Returning early drops the transaction, which rolls the designation back.
            ensure!(
                !(has_verdicts && mode.proof_verification_dev_mode),
                "refusing to designate a store that already holds verdicts for development mode; use a fresh store"
            );
            adopted_legacy = has_verdicts;
        }
        let designated = Self::designation(&mut tx)
            .await?
            .context("verifier store mode row missing after designation")?;
        tx.commit().await?;
        if &designated != mode {
            bail!("verifier store is designated as {designated:?}; refusing to start as {mode:?}");
        }
        Ok(adopted_legacy)
    }

    async fn designation(tx: &mut Connection<'_, Verifier>) -> anyhow::Result<Option<StoreMode>> {
        let row = sqlx::query(
            "SELECT proof_verification_dev_mode, bitcoin_network, bitcoin_genesis_hash \
             FROM via_verifier_store_mode WHERE id = 1",
        )
        .instrument("verifier_store_mode")
        .fetch_optional(tx)
        .await?;
        row.map(|row| {
            Ok(StoreMode {
                proof_verification_dev_mode: row.try_get("proof_verification_dev_mode")?,
                bitcoin_network: row.try_get("bitcoin_network")?,
                bitcoin_genesis_hash: row.try_get("bitcoin_genesis_hash")?,
            })
        })
        .transpose()
    }
}
