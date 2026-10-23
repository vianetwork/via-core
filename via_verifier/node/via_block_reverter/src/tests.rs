use bitcoin::{hashes::Hash, Address, Amount, Network, ScriptBuf, WPubkeyHash};
use via_verifier_dal::via_transactions_dal::L1ToL2Transaction;
use via_verifier_types::withdrawal::{CompleteWithdrawalBatch, WithdrawalRequest};
use zksync_db_connection::instrument::InstrumentExt;
use zksync_types::{Address as L2Address, H256};

use super::*;

fn wallet() -> ScriptBuf {
    ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([7; 20]))
}

fn source_hash(height: i64) -> bitcoin::BlockHash {
    bitcoin::BlockHash::from_byte_array([u8::try_from(height).unwrap(); 32])
}

async fn proof_id(storage: &mut Connection<'_, Verifier>, proof: &[u8]) -> i64 {
    storage.via_votes_dal().get_votable_transaction_id(proof).await.unwrap().unwrap()
}

async fn recover(
    pool: ConnectionPool<Verifier>,
    storage: &mut Connection<'_, Verifier>,
    source: i64,
    batch: i64,
) -> ViaVerifierBlockReverter {
    storage.via_l1_block_dal().insert_reorg_metadata(source, batch).await.unwrap();
    let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
    reverter.loop_iteration(storage).await.unwrap();
    reverter
}

async fn completed_approval(storage: &mut Connection<'_, Verifier>, id: i64) {
    let inputs = storage.via_votes_dal().verification_snapshot(id, 0).await.unwrap().unwrap();
    storage.via_votes_dal().mark_proof_verified(id, &inputs).await.unwrap();
}

async fn record_split_quorum(
    storage: &mut Connection<'_, Verifier>,
    id: i64,
    verifiers: &[String],
) {
    for verifier in &verifiers[..2] {
        observe_vote(storage, id, verifier, true, Some((9, source_hash(9)))).await.unwrap();
    }
    accept_source(storage, 10, source_hash(10)).await.unwrap();
    observe_vote(storage, id, &verifiers[2], true, Some((10, source_hash(10)))).await.unwrap();
}

async fn seed_verifiers(storage: &mut Connection<'_, Verifier>, count: u8) -> Vec<String> {
    use zksync_types::via_wallet::{SystemWalletsDetails, WalletInfo, WalletRole};

    let verifiers: Vec<_> = (0..count)
        .map(|index| {
            Address::from_script(
                &ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([30 + index; 20])),
                Network::Regtest,
            )
            .unwrap()
        })
        .collect();
    let bridge = Address::from_script(&wallet(), Network::Regtest).unwrap();
    let wallets = SystemWalletsDetails(
        WalletRole::all_roles()
            .iter()
            .map(|&role| {
                let addresses = if role == WalletRole::Verifier {
                    verifiers.clone()
                } else {
                    vec![bridge.clone()]
                };
                (
                    role,
                    WalletInfo {
                        addresses,
                        txid: bitcoin::Txid::all_zeros(),
                    },
                )
            })
            .collect(),
    );
    storage
        .via_wallet_dal()
        .insert_bootstrap_wallets(&wallets, 0)
        .await
        .unwrap();
    verifiers.iter().map(ToString::to_string).collect()
}

async fn initialize_progress(
    storage: &mut Connection<'_, Verifier>,
    module: &str,
    height: u32,
) -> anyhow::Result<()> {
    let mut tx = storage.start_transaction().await?;
    tx.via_indexer_dal().init_indexer_metadata(module, height).await?;
    tx.via_indexer_dal().update_last_processed_l1_block(module, height).await?;
    tx.commit().await?;
    Ok(())
}

async fn accept_source(
    storage: &mut Connection<'_, Verifier>,
    height: i64,
    hash: bitcoin::BlockHash,
) -> anyhow::Result<()> {
    let mut tx = storage.start_transaction().await?;
    tx.via_indexer_dal().lock_source_writer().await?;
    tx.via_l1_block_dal()
        .accept_header(via_reorg::chain::Header {
            height: u32::try_from(height)?,
            block_hash: hash,
            parent_hash: bitcoin::BlockHash::all_zeros(),
        })
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn observe_vote(
    storage: &mut Connection<'_, Verifier>,
    id: i64,
    verifier: &str,
    approved: bool,
    source: Option<(u32, bitcoin::BlockHash)>,
) -> anyhow::Result<()> {
    use bitcoin::Txid;
    let (height, hash) =
        source.ok_or_else(|| anyhow::anyhow!("vote fixture requires checked source"))?;
    let mut tx = storage.start_transaction().await?;
    tx.via_indexer_dal().lock_source_writer().await?;
    let raw:Vec<u8>=sqlx::query_scalar("SELECT e.proof_txid FROM via_proof_verdicts v JOIN via_proof_inclusions i ON i.id=v.reader_proof_inclusion_id JOIN via_proof_evidence e ON e.id=i.proof_id WHERE v.id=$1").bind(id).fetch_one(tx.conn()).await?;
    let members:String=sqlx::query_scalar("SELECT w.address FROM via_wallets w JOIN via_proof_inclusions i ON i.verifier_set_id=w.id JOIN via_proof_verdicts v ON v.reader_proof_inclusion_id=i.id WHERE v.id=$1").bind(id).fetch_one(tx.conn()).await?;
    let index =
        members.split(',').position(|member| member == verifier).unwrap_or(1000) as u32 + 100;
    let identity = Txid::from_byte_array(
        bitcoin::hashes::sha256::Hash::hash(format!("{id}:{verifier}:{approved}").as_bytes())
            .to_byte_array(),
    );
    tx.via_votes_dal()
        .record_reader_vote(
            identity,
            Txid::from_slice(&raw)?,
            verifier,
            approved,
            &serde_json::json!({"fixture":true}),
            via_reorg::chain::MessageRef {
                block: via_reorg::chain::BlockRef { height, block_hash: hash },
                carrier_txid: identity,
                tx_index: index,
                message_index: 0,
            },
        )
        .await?;
    tx.commit().await?;
    Ok(())
}

// jscpd:ignore-start Each owning crate exercises the production DAL finalizer in a real transaction; shared test-utils cannot depend back on DAL.
async fn finalize_proof(
    storage: &mut Connection<'_, Verifier>,
    id: i64,
    threshold: f64,
) -> anyhow::Result<bool> {
    let mut tx = storage.start_transaction().await?;
    let result = tx.via_votes_dal().finalize_transaction_if_needed(id, threshold).await?;
    tx.commit().await?;
    Ok(result)
}
// jscpd:ignore-end

async fn observed_votes(
    storage: &mut Connection<'_, Verifier>,
    id: i64,
) -> anyhow::Result<(i64, i64, i64)> {
    Ok(sqlx::query_as("SELECT count(*) FILTER(WHERE NOT e.approved),count(*) FILTER(WHERE e.approved),count(*) FROM via_vote_evidence e JOIN via_vote_inclusions i ON i.vote_id=e.id JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash WHERE i.revoked_at IS NULL AND e.proof_txid=(SELECT proof.proof_txid FROM via_proof_verdicts v JOIN via_proof_inclusions p ON p.id=v.reader_proof_inclusion_id JOIN via_proof_evidence proof ON proof.id=p.proof_id WHERE v.id=$1)")
        .bind(id).fetch_one(storage.conn()).await?)
}

async fn import_batch(
    storage: &mut Connection<'_, Verifier>,
    number: u32,
    deposit_height: Option<i64>,
    source_height: i64,
) -> (CompleteWithdrawalBatch, Vec<u8>) {
    let hash = H256::from_low_u64_be(u64::from(number));
    let batch = CompleteWithdrawalBatch {
        batch_number: number,
        chain_id: 270,
        network: Network::Regtest,
        protocol_version: 1,
        blob_id: format!("blob-{number}"),
        pubdata_hash: H256::repeat_byte(9),
        start_block: u64::from(number),
        end_block: u64::from(number),
        pubdata: vec![1, 2, 3],
        receipts: vec![],
        withdrawals: vec![WithdrawalRequest {
            id: format!("{number:020x}"),
            receiver: Address::from_script(
                &ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([8; 20])),
                Network::Regtest,
            )
            .unwrap(),
            amount: Amount::from_sat(10_000),
            l2_sender: L2Address::repeat_byte(3),
            l2_tx_hash: hash,
            l2_tx_log_index: 0,
        }],
        nonpayable: vec![],
    };
    let proof = hash.as_bytes().to_vec();
    use bitcoin::{BlockHash, Txid};
    let has_wallets: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM via_wallets WHERE role='Verifier')")
            .fetch_one(storage.conn())
            .await
            .unwrap();
    if !has_wallets {
        seed_verifiers(storage, 2).await;
    }
    let mut tx = storage.start_transaction().await.unwrap();
    tx.via_indexer_dal().lock_source_writer().await.unwrap();
    let height = u32::try_from(source_height).unwrap();
    let header = via_reorg::chain::Header {
        height,
        block_hash: source_hash(source_height),
        parent_hash: BlockHash::all_zeros(),
    };
    via_test_utils::reader_db::header(tx.conn(), header, true).await.unwrap();
    let (set_id, _) = tx.via_wallet_dal().verifier_set_before(height).await.unwrap();
    let mut raw = hash.0;
    raw.reverse();
    let identity = Txid::from_byte_array(raw);
    let details = via_test_utils::reader_db::ProofVerdict {
        batch: number,
        batch_hash: hash,
        parent_hash: hash,
        proof_txid: identity,
        proof_blob: format!("proof-{number}"),
        batch_txid: format!("pubdata-{number}"),
        batch_blob: batch.blob_id.clone(),
        da_identifier: "fixture".into(),
    };
    let id = via_test_utils::reader_db::verdict(
        tx.conn(),
        &details,
        via_reorg::chain::MessageRef {
            block: header.into(),
            carrier_txid: identity,
            tx_index: number,
            message_index: 0,
        },
        set_id,
    )
    .await
    .unwrap();
    completed_approval(&mut tx, id).await;
    sqlx::query("INSERT INTO via_proof_finalization(proof_inclusion_id,verifier_set_id,decision) SELECT i.id,i.verifier_set_id,TRUE FROM via_proof_inclusions i JOIN via_proof_verdicts v ON v.reader_proof_inclusion_id=i.id WHERE v.id=$1")
        .bind(id).execute(tx.conn()).await.unwrap();
    if let Some(deposit_height) = deposit_height {
        let deposit_header = via_reorg::chain::Header {
            height: u32::try_from(deposit_height).unwrap(),
            block_hash: source_hash(deposit_height),
            parent_hash: BlockHash::all_zeros(),
        };
        via_test_utils::reader_db::header(tx.conn(), deposit_header, true).await.unwrap();
        let carrier = Txid::from_byte_array([number as u8 + 100; 32]);
        tx.via_transactions_dal()
            .record_deposit_evidence(
                carrier,
                0,
                &serde_json::json!({"fixture":number}),
                via_reorg::chain::MessageRef {
                    block: deposit_header.into(),
                    carrier_txid: carrier,
                    tx_index: number,
                    message_index: 0,
                },
                hash,
            )
            .await
            .unwrap();
        tx.via_transactions_dal()
            .insert_transaction(L1ToL2Transaction {
                priority_id: i64::from(number),
                tx_id: hash,
                receiver: "fixture".into(),
                value: 10_000,
                calldata: vec![],
                canonical_tx_hash: hash,
                l1_block_number: deposit_height,
            })
            .await
            .unwrap();
        tx.via_transactions_dal().update_transaction(&hash, true, i64::from(number)).await.unwrap();
    }
    tx.commit().await.unwrap();
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    (batch, proof)
}

#[tokio::test]
async fn hard_reorg_recomputes_first_affected_batch_before_removing_sources() {
    // One affected batch catches the inclusive boundary; several catch legacy
    // metadata that names the greatest affected batch instead of the first.
    for last_affected in [2_u32, 4] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let (predecessor, predecessor_proof) = import_batch(&mut storage, 1, Some(9), 9).await;
        let mut affected = Vec::new();
        for number in 2..=last_affected {
            affected.push(
                import_batch(&mut storage, number, Some(10 + i64::from(number - 2)), 9).await,
            );
        }
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            std::iter::once(&predecessor)
                .chain(affected.iter().map(|(batch, _)| batch))
                .flat_map(|batch| batch.withdrawals.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            storage
                .via_transactions_dal()
                .get_l1_batch_number_affected_by_reorg(9)
                .await
                .unwrap(),
            Some(2)
        );
        for height in 9..=12 {
            accept_source(&mut storage,height,source_hash(height))
                .await
                .unwrap();
        }
        initialize_progress(&mut storage,"via_btc_watch", 12)
            .await
            .unwrap();
        storage
            .via_l1_block_dal()
            .insert_reorg_metadata(10, i64::from(last_affected))
            .await
            .unwrap();

        let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
        reverter.loop_iteration(&mut storage).await.unwrap();

        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            predecessor.withdrawals
        );
        assert!(storage
            .via_votes_dal()
            .proof_reveal_tx_exists(&predecessor_proof)
            .await
            .unwrap());
        assert!(storage
            .via_transactions_dal()
            .transaction_exists_with_txid(&H256::from_low_u64_be(1))
            .await
            .unwrap());
        for (batch, proof) in &affected {
            assert!(!storage
                .via_votes_dal()
                .proof_reveal_tx_exists(proof)
                .await
                .unwrap());
            assert!(!storage
                .via_transactions_dal()
                .transaction_exists_with_txid(&H256::from_low_u64_be(u64::from(batch.batch_number)))
                .await
                .unwrap());
            assert!(storage
                .via_withdrawal_dal()
                .import_complete_batch(wallet().as_bytes(), proof, batch)
                .await
                .is_err());
        }
        assert_eq!(
            storage
                .via_l1_block_dal()
                .list_l1_blocks(9, 10)
                .await
                .unwrap(),
            vec![(9, source_hash(9).to_string())]
        );
        assert_eq!(
            storage
                .via_indexer_dal()
                .get_last_processed_l1_block("via_btc_watch")
                .await
                .unwrap(),
            9
        );
        assert_eq!(
            storage
                .via_l1_block_dal()
                .has_reorg_in_progress()
                .await
                .unwrap(),
            None
        );
        // A stale import failure must not revoke the surviving predecessor.
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            predecessor.withdrawals
        );
    }
}

#[tokio::test]
async fn reorg_without_affected_deposits_preserves_source_authority() {
    // A soft event preserves authority; a hard event with missing transaction
    // evidence cannot safely infer a source boundary and must remain pending.
    for metadata in [0, 1] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let (batch, proof) = import_batch(&mut storage, 1, Some(9), 9).await;
        storage
            .via_l1_block_dal()
            .insert_reorg_metadata(10, metadata)
            .await
            .unwrap();
        let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
        let result = reverter.loop_iteration(&mut storage).await;
        if metadata == 0 {
            result.unwrap();
            assert_eq!(
                storage
                    .via_l1_block_dal()
                    .has_reorg_in_progress()
                    .await
                    .unwrap(),
                None
            );
        } else {
            assert!(result.is_err());
            assert_eq!(
                storage
                    .via_l1_block_dal()
                    .has_reorg_in_progress()
                    .await
                    .unwrap(),
                Some((10, metadata))
            );
        }
        let retained:i64=sqlx::query_scalar("SELECT count(*) FROM via_proof_verdicts WHERE proof_reveal_tx_id=$1").bind(&proof).fetch_one(storage.conn()).await.unwrap();
        assert_eq!(retained,1);
        assert_eq!(storage.via_votes_dal().proof_reveal_tx_exists(&proof).await.unwrap(),metadata==0);
        assert_eq!(storage.via_withdrawal_dal().list_eligible_withdrawals(wallet().as_bytes(),0,10).await.unwrap(),if metadata==0 {batch.withdrawals}else{vec![]});
    }
}

#[tokio::test]
async fn proof_or_vote_reorg_invalidates_sources_before_deposits_or_without_deposits() {
    // Proof-only and vote-only divergence must both find batch 2, even when
    // deposits point to batch 3 or do not exist at all.
    for (vote_only, later_deposit) in [(false, true), (true, true), (false, false), (true, false)] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let (predecessor, predecessor_proof) = import_batch(&mut storage, 1, None, 9).await;
        let members=seed_verifiers(&mut storage,2).await;
        let predecessor_id=proof_id(&mut storage,&predecessor_proof).await;
        for member in &members {
            observe_vote(&mut storage,predecessor_id,member,true,Some((9,source_hash(9)))).await.unwrap();
        }
        let first = import_batch(&mut storage, 2, Some(9), if vote_only { 9 } else { 10 }).await;
        let descendant = import_batch(&mut storage, 3, later_deposit.then_some(11), 9).await;
        if vote_only {
            let verifiers = seed_verifiers(&mut storage, 2).await;
            accept_source(&mut storage,10,source_hash(10))
                .await
                .unwrap();
            let source_id = storage
                .via_votes_dal()
                .get_votable_transaction_id(&first.1)
                .await
                .unwrap()
                .unwrap();
            observe_vote(&mut storage,source_id, &verifiers[0], true, Some((10, source_hash(10))))
                .await
                .unwrap();
        }
        assert_eq!(
            storage
                .via_transactions_dal()
                .get_l1_batch_number_affected_by_reorg(9)
                .await
                .unwrap(),
            later_deposit.then_some(3)
        );
        assert_eq!(
            storage
                .via_votes_dal()
                .get_l1_batch_number_affected_by_source_reorg(9)
                .await
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            [
                predecessor.withdrawals.clone(),
                first.0.withdrawals.clone(),
                descendant.0.withdrawals.clone()
            ]
            .concat()
        );
        initialize_progress(&mut storage,"via_btc_watch", 11)
            .await
            .unwrap();
        // Zero metadata must not hide source evidence; nonzero metadata may be
        // the later deposit batch, not the first affected source.
        storage
            .via_l1_block_dal()
            .insert_reorg_metadata(10, if later_deposit { 3 } else { 0 })
            .await
            .unwrap();
        let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
        reverter.loop_iteration(&mut storage).await.unwrap();
        // The canonical deposit remains spendable for reassignment when its
        // proof chain is deleted, and keeps its assignment on vote-only reorgs.
        assert!(storage
            .via_transactions_dal()
            .transaction_exists_with_txid(&H256::from_low_u64_be(2))
            .await
            .unwrap());
        assert_eq!(
            storage
                .via_transactions_dal()
                .deposit_prefix_for_batch(-1,10)
                .await
                .unwrap(),
            if vote_only {
                vec![]
            } else {
                vec![first.1.clone()]
            }
        );
        for (batch, proof) in [&first, &descendant] {
            let retained = vote_only && (batch.batch_number == 2 || !later_deposit);
            assert_eq!(
                storage
                    .via_votes_dal()
                    .proof_reveal_tx_exists(proof)
                    .await
                    .unwrap(),
                retained
            );
            // The affected vote parent cannot authorize imports until votes
            // finalize again. A canonical descendant survives unless the
            // independent deposit boundary removes its proof chain.
            if batch.batch_number == 2 || !retained {
                assert!(storage
                    .via_withdrawal_dal()
                    .import_complete_batch(wallet().as_bytes(), proof, batch)
                    .await
                    .is_err());
            }
        }
        // A stale scan of the removed source block cannot recreate authority.
        assert_eq!(storage.via_votes_dal().get_votable_transaction_id(H256::from_low_u64_be(2).as_bytes()).await.unwrap().is_some(),vote_only);
        assert!(storage
            .via_votes_dal()
            .proof_reveal_tx_exists(&predecessor_proof)
            .await
            .unwrap());
        assert_eq!(
            storage
                .via_withdrawal_dal()
                .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
                .await
                .unwrap(),
            predecessor.withdrawals
        );
        assert_eq!(
            storage
                .via_l1_block_dal()
                .has_reorg_in_progress()
                .await
                .unwrap(),
            None
        );
    }
}

#[tokio::test]
async fn source_batch_zero_is_not_confused_with_soft_reorg_metadata() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let zero = import_batch(&mut storage, 0, None, 10).await;
    let successor = import_batch(&mut storage, 1, None, 9).await;
    initialize_progress(&mut storage, "via_btc_watch", 10).await.unwrap();
    let _reverter = recover(pool, &mut storage, 10, 0).await;
    for (batch, proof) in [&zero, &successor] {
        assert!(!storage
            .via_votes_dal()
            .proof_reveal_tx_exists(proof)
            .await
            .unwrap());
        assert!(storage
            .via_withdrawal_dal()
            .import_complete_batch(wallet().as_bytes(), proof, batch)
            .await
            .is_err());
    }
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        storage
            .via_l1_block_dal()
            .has_reorg_in_progress()
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn vote_only_reorg_retains_canonical_proofs_and_resumes_finalization() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let verifiers = seed_verifiers(&mut storage, 2).await;
    let (batch, proof) = import_batch(&mut storage, 1, None, 9).await;
    let (successor, successor_proof) = import_batch(&mut storage, 2, None, 9).await;
    let successor_id = proof_id(&mut storage, &successor_proof).await;
    for member in &verifiers {
        observe_vote(&mut storage, successor_id, member, true, Some((9, source_hash(9))))
            .await
            .unwrap();
    }
    accept_source(&mut storage, 10, source_hash(10)).await.unwrap();
    let id = proof_id(&mut storage, &proof).await;
    observe_vote(&mut storage, id, &verifiers[0], true, Some((9, source_hash(9)))).await.unwrap();
    observe_vote(&mut storage, id, &verifiers[1], true, Some((10, source_hash(10)))).await.unwrap();
    initialize_progress(&mut storage, "via_btc_watch", 10).await.unwrap();
    let _reverter = recover(pool, &mut storage, 10, 1).await;
    assert!(storage
        .via_votes_dal()
        .proof_reveal_tx_exists(&proof)
        .await
        .unwrap());
    assert!(storage
        .via_votes_dal()
        .proof_reveal_tx_exists(&successor_proof)
        .await
        .unwrap());
    assert!(storage
        .via_withdrawal_dal()
        .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        observed_votes(&mut storage,id).await.unwrap(),
        (0, 1, 1)
    );
    assert!(!finalize_proof(&mut storage, id, BATCH_FINALIZATION_THRESHOLD).await.unwrap());
    assert_eq!(
        storage
            .via_indexer_dal()
            .get_last_processed_l1_block("via_btc_watch")
            .await
            .unwrap(),
        9
    );
    let replacement = source_hash(20);
    accept_source(&mut storage, 10, replacement).await.unwrap();
    observe_vote(&mut storage, id, &verifiers[1], true, Some((10, replacement))).await.unwrap();
    assert_eq!(
        observed_votes(&mut storage,id).await.unwrap(),
        (0, 2, 2)
    );
    initialize_progress(&mut storage, "via_btc_watch", 10).await.unwrap();
    assert!(finalize_proof(&mut storage, id, BATCH_FINALIZATION_THRESHOLD).await.unwrap());
    for (batch, proof) in [(&batch, &proof), (&successor, &successor_proof)] {
        storage
            .via_withdrawal_dal()
            .import_complete_batch(wallet().as_bytes(), proof, batch)
            .await
            .unwrap();
    }
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap(),
        [batch.withdrawals, successor.withdrawals].concat()
    );
}

#[tokio::test]
async fn redundant_vote_reorg_finalizes_retained_quorum_without_replacement() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let verifiers = seed_verifiers(&mut storage, 3).await;
    let (batch, proof) = import_batch(&mut storage, 1, None, 9).await;
    let id = proof_id(&mut storage, &proof).await;
    record_split_quorum(&mut storage, id, &verifiers).await;
    initialize_progress(&mut storage, "via_btc_watch", 10).await.unwrap();
    let _reverter = recover(pool, &mut storage, 10, 1).await;
    assert_eq!(
        observed_votes(&mut storage,id).await.unwrap(),
        (0, 2, 2)
    );
    // No new vote or explicit finalizer call: the retained two-of-three quorum
    // must restore source finalization within the reorg transaction.
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    assert_eq!(
        storage
            .via_withdrawal_dal()
            .list_eligible_withdrawals(wallet().as_bytes(), 0, 10)
            .await
            .unwrap(),
        batch.withdrawals
    );
}

#[tokio::test]
async fn rejected_sibling_vote_reorg_preserves_canonical_replacement() {
    for replacement_finalized in [None, Some(true), Some(false)] {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut storage = pool.connection().await.unwrap();
        let verifiers = seed_verifiers(&mut storage, 2).await;
        let (batch, proof) = import_batch(&mut storage, 2, None, 9).await;
        let rejected_proof = H256::repeat_byte(88);
        let mut tx=storage.start_transaction().await.unwrap();tx.via_indexer_dal().lock_source_writer().await.unwrap();
        let (set_id,_)=tx.via_wallet_dal().verifier_set_before(9).await.unwrap();
        let identity=bitcoin::Txid::from_byte_array([88;32]);
        let details=via_test_utils::reader_db::ProofVerdict { batch:2,batch_hash:rejected_proof,parent_hash:H256::zero(),proof_txid:identity,proof_blob:"rejected-proof".into(),batch_txid:"rejected-pubdata-tx".into(),batch_blob:"rejected-pubdata".into(),da_identifier:"fixture".into() };
        let id=via_test_utils::reader_db::verdict(tx.conn(),&details,via_reorg::chain::MessageRef { block:via_reorg::chain::BlockRef { height:9,block_hash:source_hash(9) },carrier_txid:identity,tx_index:88,message_index:0 },set_id).await.unwrap();
        completed_approval(&mut tx,id).await;
        sqlx::query("INSERT INTO via_proof_finalization(proof_inclusion_id,verifier_set_id,decision) SELECT i.id,i.verifier_set_id,FALSE FROM via_proof_inclusions i JOIN via_proof_verdicts v ON v.reader_proof_inclusion_id=i.id WHERE v.id=$1")
            .bind(id).execute(tx.conn()).await.unwrap();
        sqlx::query("UPDATE via_proof_finalization SET decision=$1 WHERE proof_inclusion_id IN(SELECT reader_proof_inclusion_id FROM via_proof_verdicts WHERE proof_reveal_tx_id=$2)")
            .bind(replacement_finalized).bind(&proof).execute(tx.conn()).await.unwrap();tx.commit().await.unwrap();
        let rejected_id = storage
            .via_votes_dal()
            .get_votable_transaction_id(rejected_proof.as_bytes())
            .await
            .unwrap()
            .unwrap();
        observe_vote(&mut storage,rejected_id, &verifiers[0], false, Some((9, source_hash(9))))
            .await
            .unwrap();
        accept_source(&mut storage,10,source_hash(10))
            .await
            .unwrap();
        observe_vote(&mut storage,
                rejected_id,
                &verifiers[1],
                false,
                Some((10, source_hash(10))),
            )
            .await
            .unwrap();
        storage.via_l1_block_dal().insert_reorg_metadata(10,2).await.unwrap();
        let mut reverter = ViaVerifierBlockReverter::new(pool, ViaReorgDetectorConfig::for_tests());
        reverter.loop_iteration(&mut storage).await.unwrap();
        for (source, expected) in [
            (rejected_proof.as_bytes(), Some(false)),
            (proof.as_slice(), replacement_finalized),
        ] {
            let actual: Option<bool> = sqlx::query_scalar(
                "SELECT is_finalized FROM via_votable_transactions WHERE proof_reveal_tx_id=$1",
            )
            .bind(source)
            .instrument("reverter_sibling_finality")
            .fetch_one(&mut storage)
            .await
            .unwrap();
            assert_eq!(actual, expected);
        }
        assert_eq!(
            observed_votes(&mut storage,rejected_id)
                .await
                .unwrap(),
            (1, 0, 1)
        );
        assert_eq!(
            storage
                .via_l1_block_dal()
                .has_reorg_in_progress()
                .await
                .unwrap(),
            None
        );
        let restored = storage
            .via_withdrawal_dal()
            .import_complete_batch(wallet().as_bytes(), &proof, &batch)
            .await;
        assert_eq!(restored.is_ok(), replacement_finalized == Some(true));
    }
}

#[tokio::test]
async fn vote_reorg_keeps_completed_descendant_when_legacy_parent_result_is_negative() {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let mut storage = pool.connection().await.unwrap();
    let verifiers = seed_verifiers(&mut storage, 3).await;
    let (_, parent_proof) = import_batch(&mut storage, 1, None, 9).await;
    let (batch, proof) = import_batch(&mut storage, 2, None, 9).await;
    let id = proof_id(&mut storage, &proof).await;
    record_split_quorum(&mut storage, id, &verifiers).await;
    sqlx::query("UPDATE via_proof_verdicts SET l1_batch_status=FALSE WHERE proof_reveal_tx_id=$1")
        .bind(&parent_proof)
        .execute(storage.conn())
        .await
        .unwrap();
    let _reverter = recover(pool, &mut storage, 10, 2).await;
    assert_eq!(
        observed_votes(&mut storage,id).await.unwrap(),
        (0, 2, 2)
    );
    assert_eq!(
        storage
            .via_votes_dal()
            .get_first_rejected_l1_batch()
            .await
            .unwrap(),
        None
    );
    storage
        .via_withdrawal_dal()
        .import_complete_batch(wallet().as_bytes(), &proof, &batch)
        .await
        .unwrap();
    let retained: i64 =
        sqlx::query_scalar("SELECT count(*) FROM via_proof_verdicts WHERE proof_reveal_tx_id=$1")
            .bind(&proof)
            .fetch_one(storage.conn())
            .await
            .unwrap();
    assert_eq!(retained, 1);
}
