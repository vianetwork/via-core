use std::sync::Arc;

use anyhow::Context as _;
use bitcoin::Address;
use tokio::sync::watch;
use via_btc_client::traits::BitcoinOps;
use via_verifier_dal::{ConnectionPool, Verifier};
use via_withdrawal_client::client::WithdrawalClient;
use zksync_config::configs::via_verifier::ViaVerifierConfig;

use crate::coordinator::api_decl::RestApi;

#[allow(clippy::too_many_arguments)]
pub async fn start_coordinator_server(
    config: ViaVerifierConfig,
    master_connection_pool: ConnectionPool<Verifier>,
    btc_client: Arc<dyn BitcoinOps>,
    withdrawal_client: WithdrawalClient,
    bridge_address: Address,
    verifiers_pub_keys: Vec<String>,
    required_signers: usize,
    mut stop_receiver: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let bind_address = config.bind_addr();
    let api = RestApi::new(
        config,
        master_connection_pool,
        btc_client,
        withdrawal_client,
        bridge_address,
        verifiers_pub_keys,
        required_signers,
    )?
    .into_router();

    let listener = tokio::net::TcpListener::bind(bind_address)
        .await
        .context("Cannot bind to the specified address")?;
    axum::serve(listener, api)
        .with_graceful_shutdown(async move {
            if stop_receiver.changed().await.is_err() {
                tracing::warn!("Stop signal sender for coordinator server was dropped without sending a signal");
            }
            tracing::info!("Stop signal received, coordinator server is shutting down");
        })
        .await
        .with_context(|| "coordinator handler server failed")?;
    tracing::info!("coordinator handler server shut down");
    Ok(())
}
