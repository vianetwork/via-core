mod genesis;
mod indexer;
pub mod wallets;

use std::sync::Arc;

use bitcoin::blockdata::constants::genesis_block;
use genesis::VerifierGenesis;
use via_btc_client::{bootstrap::ViaBootstrap, client::BitcoinClient, traits::BitcoinOps};
use via_verifier_dal::{via_store_mode_dal::StoreMode, ConnectionPool, Verifier, VerifierDal};
use wallets::ViaWalletsInitializer;
use zksync_config::{configs::via_consensus::ViaGenesisConfig, ViaBtcWatchConfig};

use crate::indexer::ViaIndexerInitializer;

#[derive(Debug, Clone)]
pub struct ViaVerifierStorageInitializer {}

impl ViaVerifierStorageInitializer {
    pub async fn new(
        pool: ConnectionPool<Verifier>,
        client: Arc<BitcoinClient>,
        via_genesis_config: ViaGenesisConfig,
        btc_watch_config: ViaBtcWatchConfig,
        proof_verification_dev_mode: bool,
    ) -> anyhow::Result<Self> {
        // Runs on every start, before any verifier service can read or write verdicts.
        // The configured network name falls back to regtest when unparseable, so the node's block 0 must match it.
        // This separates Bitcoin networks, not two regtest deployments, which share one genesis.
        let network = client.get_network();
        let genesis_hash = client.fetch_block(0).await?.block_hash();
        let expected_genesis = genesis_block(network).block_hash();
        anyhow::ensure!(
            genesis_hash == expected_genesis,
            "Bitcoin node genesis {genesis_hash} does not match configured network {network} ({expected_genesis})"
        );
        pool.connection()
            .await?
            .via_store_mode_dal()
            .ensure_proof_verification_mode(&StoreMode {
                proof_verification_dev_mode,
                bitcoin_network: network.to_string(),
                bitcoin_genesis_hash: genesis_hash.to_string(),
            })
            .await?;

        // Check if already initialized
        if pool
            .connection()
            .await?
            .via_protocol_versions_dal()
            .latest_protocol_semantic_version()
            .await?
            .is_some()
        {
            tracing::info!("Verifier storage already initialized");
            return Ok(Self {});
        }

        let bootstrap = ViaBootstrap::new(client, via_genesis_config);
        let bootstrap_state = bootstrap.process_bootstrap_messages().await?;

        let genesis = Arc::new(VerifierGenesis {
            bootstrap: bootstrap_state.clone(),
            pool: pool.clone(),
        });

        let indexer =
            ViaIndexerInitializer::new(pool.clone(), bootstrap_state.clone(), btc_watch_config);
        let wallets = ViaWalletsInitializer::new(pool, bootstrap_state);

        genesis.initialize_storage().await?;
        wallets.initialize_storage().await?;
        indexer.initialize_storage().await?;

        Ok(Self {})
    }
}
