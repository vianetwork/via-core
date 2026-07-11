use std::{
    env,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use bitcoin::{Network, ScriptBuf};
use sqlx::{postgres::PgPoolOptions, PgPool};
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps, types::NodeAuth};
use via_btc_ingestion::{ProtocolContext, ProtocolVersionTag, Role, WalletSet};
use via_ingestion_shadow::{execute, Mode, RunConfig};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

fn split_database_url(url: &str) -> (&str, &str) {
    url.rsplit_once('/').expect("TEST_DATABASE_URL must contain a database name")
}

async fn admin_pool(base: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .connect(&format!("{base}/postgres"))
        .await
        .expect("connect to Postgres admin database")
}

async fn clone_dev_test() -> (String, PgPool, String) {
    let template_url = env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must provide compose credentials for the dev_test template");
    let (base, _) = split_database_url(&template_url);
    let database = format!("via_shadow_e2e_{}_{}", std::process::id(), DB_COUNTER.fetch_add(1, Ordering::SeqCst));
    let admin = admin_pool(base).await;
    for attempt in 0..20 {
        let result = sqlx::query(&format!("CREATE DATABASE \"{database}\" TEMPLATE dev_test")).execute(&admin).await;
        match result {
            Ok(_) => break,
            Err(err)
                if attempt < 19
                    && err.as_database_error().and_then(|error| error.code()).is_some_and(|code| code == "55006") =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            Err(err) => panic!("clone dev_test template: {err}"),
        }
    }
    (format!("{base}/{database}"), admin, database)
}

async fn drop_database(admin: &PgPool, database: &str) {
    sqlx::query(&format!("DROP DATABASE \"{database}\" WITH (FORCE)"))
        .execute(admin)
        .await
        .expect("drop throwaway shadow database");
}

fn empty_context() -> ProtocolContext {
    ProtocolContext {
        version: 1,
        wallets: WalletSet {
            sequencer: ScriptBuf::new(),
            bridge: ScriptBuf::new(),
            governance: ScriptBuf::new(),
            verifiers: vec![],
        },
        protocol_version: ProtocolVersionTag { minor: 0, patch: 0 },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_coinbase_only_range_and_audit_coverage() {
    let Ok(rpc_url) = env::var("VIA_SHADOW_E2E_RPC_URL") else {
        println!("skipped: VIA_SHADOW_E2E_RPC_URL is unset");
        return;
    };
    let rpc_user =
        env::var("VIA_BTC_CLIENT_RPC_USER").expect("VIA_BTC_CLIENT_RPC_USER must be set for the shadow E2E test");
    let rpc_password = env::var("VIA_BTC_CLIENT_RPC_PASSWORD")
        .expect("VIA_BTC_CLIENT_RPC_PASSWORD must be set for the shadow E2E test");
    let client = Arc::new(
        BitcoinClient::new(&rpc_url, NodeAuth::UserPass(rpc_user, rpc_password), ViaBtcClientConfig::for_tests())
            .expect("construct regtest Bitcoin client"),
    );
    for height in 1..=3 {
        let block = match tokio::time::timeout(std::time::Duration::from_secs(30), client.fetch_block(height)).await {
            Ok(Ok(block)) => block,
            Ok(Err(err)) => {
                println!("skipped: local regtest node is unreachable: {err}");
                return;
            }
            Err(_) => {
                println!("skipped: timed out waiting for local regtest node");
                return;
            }
        };
        assert_eq!(block.txdata.len(), 1, "height {height} is not coinbase-only");
        assert!(block.txdata[0].is_coinbase(), "height {height} does not contain a coinbase transaction");
    }

    let (database_url, admin, database) = clone_dev_test().await;
    let mut replay_task = tokio::spawn(execute(RunConfig {
        client,
        network: Network::Regtest,
        database_url,
        role: Role::CoreSequencer,
        from_height: 1,
        to_height: 3,
        mode: Mode::Replay,
        out: None,
        bootstrap_context: Some(empty_context()),
    }));
    let execution = match tokio::time::timeout(std::time::Duration::from_secs(120), &mut replay_task).await {
        Ok(execution) => Some(execution),
        Err(_) => {
            replay_task.abort();
            let _ = replay_task.await;
            None
        }
    };
    drop_database(&admin, &database).await;
    admin.close().await;

    let execution = execution.unwrap_or_else(|| panic!("shadow replay timed out"));
    let result = match execution {
        Ok(result) => result,
        Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
        Err(err) => panic!("replay task was cancelled: {err}"),
    };
    let summary = result.expect("replay coinbase-only range");
    assert_eq!(summary.blocks_applied, 3);
    assert!(summary.events_per_kind.is_empty());
    assert!(summary.rejections_per_code.is_empty());
    assert!(summary.coverage.contiguous);
    assert!(summary.coverage.unresolved_dependencies.is_empty());
}
