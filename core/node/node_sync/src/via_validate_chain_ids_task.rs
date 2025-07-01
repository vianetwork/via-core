//! Miscellaneous helpers for the EN.

use std::{sync::Arc, time::Duration};

use futures::FutureExt;
use tokio::sync::watch;
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps};
use zksync_types::{BitcoinNetwork, L2ChainId};
use zksync_web3_decl::{
    client::{DynClient, L2},
    error::ClientRpcContext,
    namespaces::{EthNamespaceClient, ViaNamespaceClient},
};

/// Task that validates chain IDs.
#[derive(Debug)]
pub struct ValidateChainIdsTask {
    btc_network: BitcoinNetwork,
    l2_chain_id: L2ChainId,
    btc_client: Arc<BitcoinClient>,
    main_node_client: Box<DynClient<L2>>,
}

impl ValidateChainIdsTask {
    const BACKOFF_INTERVAL: Duration = Duration::from_secs(5);

    pub fn new(
        btc_network: BitcoinNetwork,
        l2_chain_id: L2ChainId,
        btc_client: Arc<BitcoinClient>,
        main_node_client: Box<DynClient<L2>>,
    ) -> Self {
        Self {
            btc_network,
            l2_chain_id,
            btc_client,
            main_node_client: main_node_client.for_component("chain_ids_validation"),
        }
    }

    async fn check_btc_client(
        btc_client: Arc<BitcoinClient>,
        expected: BitcoinNetwork,
    ) -> anyhow::Result<()> {
        let network = btc_client.get_network();
        anyhow::ensure!(
                expected == network,
                "Configured L1 chain ID doesn't match the one from Bitcoin node. \
                Make sure your configuration is correct and you are corrected to the right Bitcoin node. \
                Eth node chain ID: {network}. Local config value: {expected}"
            );
        tracing::info!("Checked that L1 chain ID {network} is returned by Bitcoin client");
        return Ok(());
    }

    async fn check_l1_chain_using_main_node(
        main_node_client: Box<DynClient<L2>>,
        expected: BitcoinNetwork,
    ) -> anyhow::Result<()> {
        loop {
            match main_node_client
                .get_bitcoin_network()
                .rpc_context("get_bitcoin_network")
                .await
            {
                Ok(network) => {
                    anyhow::ensure!(
                        expected == network,
                        "Configured L1 chain ID doesn't match the one from main node. \
                        Make sure your configuration is correct and you are corrected to the right main node. \
                        Main node L1 chain ID: {network}. Local config value: {expected}"
                    );
                    tracing::info!(
                        "Checked that L1 chain ID {network} is returned by main node client"
                    );
                    return Ok(());
                }
                Err(err) if err.is_retriable() => {
                    tracing::warn!(
                        "Retriable error getting L1 chain ID from main node client, will retry in {:?}: {err}",
                        Self::BACKOFF_INTERVAL
                    );
                    tokio::time::sleep(Self::BACKOFF_INTERVAL).await;
                }
                Err(err) => {
                    tracing::error!("Error getting L1 chain ID from main node client: {err}");
                    return Err(err.into());
                }
            }
        }
    }

    async fn check_l2_chain_using_main_node(
        main_node_client: Box<DynClient<L2>>,
        expected: L2ChainId,
    ) -> anyhow::Result<()> {
        loop {
            match main_node_client.chain_id().rpc_context("chain_id").await {
                Ok(chain_id) => {
                    let chain_id = L2ChainId::try_from(chain_id.as_u64()).map_err(|err| {
                        anyhow::anyhow!("invalid chain ID supplied by main node: {err}")
                    })?;
                    anyhow::ensure!(
                        expected == chain_id,
                        "Configured L2 chain ID doesn't match the one from main node. \
                        Make sure your configuration is correct and you are corrected to the right main node. \
                        Main node L2 chain ID: {chain_id:?}. Local config value: {expected:?}"
                    );
                    tracing::info!(
                        "Checked that L2 chain ID {chain_id:?} is returned by main node client"
                    );
                    return Ok(());
                }
                Err(err) if err.is_retriable() => {
                    tracing::warn!(
                        "Transient error getting L2 chain ID from main node client, will retry in {:?}: {err}",
                        Self::BACKOFF_INTERVAL
                    );
                    tokio::time::sleep(Self::BACKOFF_INTERVAL).await;
                }
                Err(err) => {
                    tracing::error!("Error getting L2 chain ID from main node client: {err}");
                    return Err(err.into());
                }
            }
        }
    }

    /// Runs the task once, exiting either when all the checks are performed or when the stop signal is received.
    pub async fn run_once(self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        let btc_client_check = Self::check_btc_client(self.btc_client, self.btc_network);
        let main_node_l1_check =
            Self::check_l1_chain_using_main_node(self.main_node_client.clone(), self.btc_network);
        let main_node_l2_check =
            Self::check_l2_chain_using_main_node(self.main_node_client, self.l2_chain_id);
        let joined_futures =
            futures::future::try_join3(btc_client_check, main_node_l1_check, main_node_l2_check)
                .fuse();
        tokio::select! {
            res = joined_futures => res.map(drop),
            _ = stop_receiver.changed() =>  Ok(()),
        }
    }

    /// Runs the task until the stop signal is received.
    pub async fn run(self, mut stop_receiver: watch::Receiver<bool>) -> anyhow::Result<()> {
        // Since check futures are fused, they are safe to poll after getting resolved; they will never resolve again,
        // so we'll just wait for another check or a stop signal.
        let btc_client_check = Self::check_btc_client(self.btc_client, self.btc_network).fuse();
        let main_node_l1_check =
            Self::check_l1_chain_using_main_node(self.main_node_client.clone(), self.btc_network)
                .fuse();
        let main_node_l2_check =
            Self::check_l2_chain_using_main_node(self.main_node_client, self.l2_chain_id).fuse();
        tokio::select! {
            Err(err) = btc_client_check =>  Err(err),
            Err(err) = main_node_l1_check =>  Err(err),
            Err(err) = main_node_l2_check =>  Err(err),
            _ = stop_receiver.changed() =>  Ok(()),
        }
    }
}
