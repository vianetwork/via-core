//! One complete protocol block. Native deposit and proof writes precede shared completion.

use anyhow::{ensure, Context};
use bitcoin::{Address, Network};
use via_btc_client::{
    client::BitcoinClient,
    indexer::{
        configuration::{prepare_configurations, ConfigurationInput},
        replay::{Included, ProofInput, ScannedDeposit},
        BitcoinInscriptionIndexer,
    },
    types::{FullInscriptionMessage, HeaderMembership, ValidatorAttestationInput},
};
use zksync_db_connection::connection::DbMarker;
use zksync_types::via_wallet::SystemWallets;

use crate::{CanonicalProof, ReaderDal, StoredHeader, PROTOCOL_CONSUMER};

/// ProtocolBlock prepares every message, deposit, proof, configuration and vote of one block from Bitcoin before SQL.
/// complete writes configurations, votes and consumer position in caller's transaction, so block effects and position commit together or not at all.
/// Idea only from Sui indexer sequential committer, which commits handler effects and watermark in one transaction: https://github.com/MystenLabs/sui/blob/6de525b6b70277c845f4f5751a9e87fc7130dd48/crates/sui-indexer-alt-framework/src/pipeline/sequential/committer.rs
pub struct ProtocolBlock {
    pub header: StoredHeader,
    pub messages: Vec<FullInscriptionMessage>,
    pub deposits: Vec<ScannedDeposit>,
    pub proofs: Vec<Included<ProofInput>>,
    pub(crate) configurations: Vec<Included<ConfigurationInput>>,
    votes: Vec<Included<ValidatorAttestationInput>>,
}

impl ProtocolBlock {
    pub async fn prepare(
        client: &BitcoinClient,
        indexer: &mut BitcoinInscriptionIndexer,
        header: StoredHeader,
        wallets: SystemWallets,
        prepared_body: Option<&bitcoin::Block>,
    ) -> anyhow::Result<Self> {
        let source = client
            .header_by_hash(&header.header.block_hash)
            .await?
            .context("protocol block unavailable")?;
        ensure!(
            source.height == header.header.height
                && matches!(source.membership, HeaderMembership::Canonical(_)),
            "protocol block is not canonical"
        );
        let fetched;
        let body = if let Some(body) = prepared_body {
            ensure!(body.block_hash() == source.hash, "prepared protocol body changed");
            body
        } else {
            fetched = client.checked_block(&source).await?;
            &fetched
        };
        indexer.update_system_wallets(
            Some(wallets.sequencer),
            Some(wallets.bridge),
            Some(wallets.verifiers),
            Some(wallets.governance),
        );
        let messages = indexer.process_block_body(body, source.height).await?;
        let configurations = prepare_configurations(client, &messages).await?;
        let proofs = client.prepare_proofs(&messages).await?;
        let votes = messages
            .iter()
            .filter_map(|message| match message {
                FullInscriptionMessage::ValidatorAttestation(vote) => {
                    Some(Included::new(&vote.common, vote.input.clone()))
                }
                _ => None,
            })
            .collect::<anyhow::Result<_>>()?;
        let deposits = ScannedDeposit::from_messages(messages.clone())?;
        Ok(Self { header, messages, deposits, proofs, configurations, votes })
    }

    /// The parent pubdata uses the sequencer at its own height, which may precede a rotation.
    pub async fn proof_is_authorized<DB: DbMarker>(
        reader: &mut ReaderDal<'_, '_, DB>,
        proof: &Included<ProofInput>,
        network: Network,
    ) -> anyhow::Result<bool> {
        let parent = &proof.payload.input.pubdata;
        let wallets = reader
            .wallets_before(parent.origin.height)
            .await?
            .context("pubdata configuration is incomplete")?;
        let Some(script) = &parent.payload.sender_script else { return Ok(false) };
        Ok(Address::from_script(bitcoin::Script::from_bytes(script), network)? == wallets.sequencer)
    }

    pub async fn complete<DB: DbMarker>(
        &self,
        reader: &mut ReaderDal<'_, '_, DB>,
        proofs: &[CanonicalProof],
        network: Network,
        oldest_needed: &StoredHeader,
    ) -> anyhow::Result<()> {
        for change in &self.configurations {
            reader.save_configuration(change).await?;
        }
        for vote in &self.votes {
            if let Some(proof) = proofs
                .iter()
                .find(|proof| proof.work.proof.origin.txid == vote.payload.input.reference_txid)
            {
                reader.save_reader_vote(vote, &proof.work, network).await?;
            }
        }
        reader.complete_consumer(PROTOCOL_CONSUMER, &self.header, oldest_needed).await
    }
}
