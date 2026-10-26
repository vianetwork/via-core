use bitcoin::{Address, Amount, Network, PrivateKey, Txid};
use serde_json::json;
use via_btc_client::{
    inscriber::Inscriber,
    types::{
        InscriptionMessage, L1BatchDAReferenceInput, ProofDAReferenceInput,
        ValidatorAttestationInput, Vote,
    },
};
use via_btc_storage::{WalletSource, PROTOCOL_CONSUMER};
use via_reorg::chain::{BlockRef, Header};
use via_test_utils::{bitcoin_core::BitcoinCore, utils::test_wallets};
use zksync_types::{
    via_wallet::{SystemWallets, SystemWalletsDetails},
    L1BatchNumber, H256,
};

use super::*;

struct Fixture {
    core: BitcoinCore,
    client: Arc<BitcoinClient>,
    pool: ConnectionPool<Verifier>,
    wallets: SystemWallets,
    voter: Inscriber,
    proof: Txid,
    target: u32,
}
impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        let core = BitcoinCore::new()?;
        let client = core.client()?;
        let mut sequencer = Inscriber::new(
            core.inscriber_client()?,
            &PrivateKey::generate(Network::Regtest).to_wif(),
            None,
        )
        .await?;
        let mut voter = Inscriber::new(
            core.inscriber_client()?,
            &PrivateKey::generate(Network::Regtest).to_wif(),
            None,
        )
        .await?;
        core.fund(&sequencer.inscriber_address()?, Amount::from_sat(2_000_000))?;
        core.fund(&voter.inscriber_address()?, Amount::from_sat(2_000_000))?;
        let mut wallets = test_wallets();
        wallets.sequencer = sequencer.inscriber_address()?;
        wallets.verifiers = vec![voter.inscriber_address()?];
        wallets.bridge = core
            .rpc::<Address<bitcoin::address::NetworkUnchecked>>("getnewaddress", &[])?
            .require_network(Network::Regtest)?;
        core.fund(&wallets.bridge, Amount::from_sat(100_000))?;
        let anchor = core.rpc::<u32>("getblockcount", &[])?;
        let pubdata = sequencer
            .inscribe(InscriptionMessage::L1BatchDAReference(L1BatchDAReferenceInput {
                l1_batch_index: L1BatchNumber(1),
                l1_batch_hash: H256::repeat_byte(4),
                prev_l1_batch_hash: H256::zero(),
                da_identifier: "fixture".into(),
                blob_id: "batch".into(),
            }))
            .await?
            .final_reveal_tx
            .txid;
        core.mine(1)?;
        sequencer.sync_context_with_blockchain().await?;
        let proof = sequencer
            .inscribe(InscriptionMessage::ProofDAReference(ProofDAReferenceInput {
                l1_batch_reveal_txid: pubdata,
                da_identifier: "fixture".into(),
                blob_id: "proof".into(),
            }))
            .await?
            .final_reveal_tx
            .txid;
        voter
            .inscribe(InscriptionMessage::ValidatorAttestation(ValidatorAttestationInput {
                reference_txid: proof,
                attestation: Vote::Ok,
            }))
            .await?;
        let utxos: Vec<serde_json::Value> = core
            .rpc("listunspent", &[json!(1), json!(9999999), json!([wallets.bridge.to_string()])])?;
        let raw:String=core.rpc("createrawtransaction",&[json!([{"txid":utxos[0]["txid"],"vout":utxos[0]["vout"]}]),json!([{wallets.governance.to_string():0.0005},{"data":"5649415f57490001010101010101010101"},{wallets.bridge.to_string():0.0004}])])?;
        let signed: serde_json::Value = core.rpc("signrawtransactionwithwallet", &[json!(raw)])?;
        let _: Txid = core.rpc("sendrawtransaction", &[signed["hex"].clone()])?;
        let deposit:String=core.rpc("createrawtransaction",&[json!([]),json!([{wallets.bridge.to_string():0.0005},{"data":"0707070707070707070707070707070707070707"}])])?;
        let funded: serde_json::Value = core.rpc("fundrawtransaction", &[json!(deposit)])?;
        let signed: serde_json::Value =
            core.rpc("signrawtransactionwithwallet", &[funded["hex"].clone()])?;
        let _: Txid = core.rpc("sendrawtransaction", &[signed["hex"].clone()])?;
        core.mine(1)?;
        let target = anchor + 2;
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut db = pool.connection().await?;
        db.via_store_mode_dal()
            .ensure_proof_verification_mode(&via_verifier_dal::via_store_mode_dal::StoreMode {
                proof_verification_dev_mode: false,
                bitcoin_network: "regtest".into(),
                bitcoin_genesis_hash: client.canonical_header_at(0).await?.hash.to_string(),
            })
            .await?;
        let mut tx = db.start_transaction().await?;
        let genesis = client.canonical_header_at(0).await?;
        let start = client.canonical_header_at(anchor).await?;
        let mut reader = ReaderDal::new(&mut tx);
        reader
            .initialize(
                "regtest",
                genesis.hash,
                Header { height: anchor, block_hash: start.hash, parent_hash: start.parent_hash },
                BlockRef {
                    height: target,
                    block_hash: client.canonical_header_at(target).await?.hash,
                },
            )
            .await?;
        reader
            .insert_wallets(
                &SystemWalletsDetails::try_from(wallets.clone())?,
                WalletSource::Unbound(i64::from(anchor)),
            )
            .await?;
        let root = reader.header_at(anchor).await?.unwrap();
        for name in [PROTOCOL_CONSUMER, "withdrawals"] {
            reader.complete_consumer(name, &root, &root).await?;
        }
        tx.commit().await?;
        let mut indexer = BitcoinInscriptionIndexer::new(client.clone(), Arc::new(wallets.clone()));
        assert!(
            via_btc_storage::transition::process_next(&mut db, &client, &mut indexer, 0.66).await?
        );
        Ok(Self { core, client, pool, wallets, voter, proof, target })
    }
    async fn watcher(&self) -> anyhow::Result<VerifierBtcWatch> {
        VerifierBtcWatch::new(
            ViaBtcWatchConfig { poll_interval: 10, ..ViaBtcWatchConfig::for_tests() },
            BitcoinInscriptionIndexer::new(self.client.clone(), Arc::new(self.wallets.clone())),
            self.client.clone(),
            self.pool.clone(),
        )
        .await
    }
}
async fn wait_consumer(
    pool: &ConnectionPool<Verifier>,
    name: &str,
    height: u32,
) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let mut db = pool.connection().await?;
            if ReaderDal::new(&mut db).consumer(name).await?.unwrap().applied.header.height
                == height
            {
                return anyhow::Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}
#[tokio::test]
async fn verifier_protocol_loop_commits_deposit_proof_vote_and_consumer_together(
) -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let watcher = fixture.watcher().await?;
    let (stop, receiver) = watch::channel(false);
    let task = tokio::spawn(watcher.run(receiver));
    wait_consumer(&fixture.pool, PROTOCOL_CONSUMER, fixture.target).await?;
    stop.send(true)?;
    task.await??;
    let mut db = fixture.pool.connection().await?;
    let proof = db.via_votes_dal().pending_reader_proof().await?.unwrap();
    assert_eq!(proof.proof.origin.txid, fixture.proof);
    assert_eq!(ReaderDal::new(&mut db).reader_votes(fixture.target, &[proof.id]).await?.len(), 1);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM via_transactions WHERE bitcoin_height=$1")
            .bind(i64::from(fixture.target))
            .fetch_one(db.conn())
            .await?;
    assert_eq!(count, 1);
    Ok(())
}
#[tokio::test]
async fn late_consumer_failure_rolls_back_native_protocol_effects() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let mut db = fixture.pool.connection().await?;
    let parent = ReaderDal::new(&mut db).get_highest_block().await?.unwrap();
    sqlx::query(&format!(
        "ALTER TABLE via_btc_consumers ADD CONSTRAINT late_consumer_failure CHECK (header_id<={})",
        parent.id
    ))
    .execute(db.conn())
    .await?;
    let (_stop, receiver) = watch::channel(false);
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(10),
        fixture.watcher().await?.run(receiver)
    )
    .await?
    .is_err());
    assert_eq!(ReaderDal::new(&mut db).get_highest_block().await?.unwrap(), parent);
    for table in ["via_transactions", "via_votable_transactions", "via_votes"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(db.conn())
            .await?;
        assert_eq!(count, 0, "{table}");
    }
    sqlx::query("ALTER TABLE via_btc_consumers DROP CONSTRAINT late_consumer_failure")
        .execute(db.conn())
        .await?;
    let (stop, receiver) = watch::channel(false);
    let task = tokio::spawn(fixture.watcher().await?.run(receiver));
    wait_consumer(&fixture.pool, PROTOCOL_CONSUMER, fixture.target).await?;
    stop.send(true)?;
    task.await??;
    Ok(())
}
#[tokio::test]
async fn second_member_vote_stops_the_actual_reader_durably() -> anyhow::Result<()> {
    let mut fixture = Fixture::new().await?;
    let mut db = fixture.pool.connection().await?;
    let mut indexer =
        BitcoinInscriptionIndexer::new(fixture.client.clone(), Arc::new(fixture.wallets.clone()));
    via_btc_storage::transition::process_next(&mut db, &fixture.client, &mut indexer, 0.66).await?;
    fixture.voter.sync_context_with_blockchain().await?;
    fixture
        .voter
        .inscribe(InscriptionMessage::ValidatorAttestation(ValidatorAttestationInput {
            reference_txid: fixture.proof,
            attestation: Vote::NotOk,
        }))
        .await?;
    fixture.core.mine(1)?;
    let (_stop, receiver) = watch::channel(false);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        fixture.watcher().await?.run(receiver),
    )
    .await?
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<via_btc_storage::StoreError>(),
        Some(via_btc_storage::StoreError::Stopped(_))
    ));
    let mut restarted = fixture.pool.connection().await?;
    let state = ReaderDal::new(&mut restarted).state().await?.unwrap();
    assert!(state.stopped_reason.unwrap().contains("conflict"));
    assert!(!state.replay_ready);
    let proof = restarted.via_votes_dal().pending_reader_proof().await?.unwrap();
    assert_eq!(
        ReaderDal::new(&mut restarted).reader_votes(fixture.target + 1, &[proof.id]).await?.len(),
        1
    );
    Ok(())
}
#[tokio::test]
async fn withdrawal_observation_advances_while_protocol_and_proof_work_trail() -> anyhow::Result<()>
{
    let fixture = Fixture::new().await?;
    let watcher = fixture.watcher().await?;
    assert!(watcher.withdrawal_fulfillment.is_none());
    let mut db = fixture.pool.connection().await?;
    let prepared = PreparedTransition::prepare(
        fixture.client.as_ref(),
        ReaderSnapshot::load(&mut ReaderDal::new(&mut db), fixture.client.reorg_safe_depth()?)
            .await?,
    )
    .await?;
    let mut tx = db.start_transaction().await?;
    prepared.apply_headers(&mut ReaderDal::new(&mut tx)).await?;
    tx.commit().await?;
    let mut observer = withdrawal::WithdrawalObserver::new(
        fixture.client.clone(),
        watcher.indexer,
        fixture.pool.clone(),
    );
    let (stop, receiver) = watch::channel(false);
    let task =
        tokio::spawn(
            async move { observer.run(receiver, std::time::Duration::from_millis(10)).await },
        );
    wait_consumer(&fixture.pool, "withdrawals", fixture.target).await?;
    stop.send(true)?;
    task.await??;
    assert_eq!(
        ReaderDal::new(&mut db).consumer(PROTOCOL_CONSUMER).await?.unwrap().applied.header.height,
        fixture.target - 1
    );
    assert!(!ReaderDal::new(&mut db).state().await?.unwrap().replay_ready);
    let observations = db
        .via_withdrawal_dal()
        .pending_withdrawal_observations(fixture.wallets.bridge.script_pubkey().as_bytes())
        .await?;
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].inclusion.as_ref().unwrap().block_height, fixture.target);
    Ok(())
}
#[tokio::test]
async fn stale_fulfillment_policy_holds_before_source_rpc() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let mut watcher = fixture.watcher().await?.with_withdrawal_fulfillment(
        TransactionBuilderConfig::withdrawal(fixture.wallets.governance.clone()),
        1,
    )?;
    fixture.core.rpc::<serde_json::Value>("stop", &[])?;
    let mut db = fixture.pool.connection().await?;
    watcher.reconcile_withdrawals(&mut db).await?;
    watcher.withdrawal_fulfillment.as_mut().unwrap().config.bridge_address =
        fixture.wallets.bridge.clone();
    assert!(watcher.reconcile_withdrawals(&mut db).await.is_err());
    Ok(())
}
