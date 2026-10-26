use bitcoin::{hashes::Hash, Txid};
use via_btc_client::{
    indexer::replay::{Included, MessageOrigin, Payload},
    inscriber::test_utils::{get_mock_inscriber_and_conditions, MockBitcoinOps},
    traits::Serializable,
    types::{BlockHeaderInfo, HeaderMembership, InscriptionMessage, ProofDAReferenceInput},
};
use via_btc_storage::{ReaderDal, WalletSource};
use via_reorg::chain::Header;
use via_test_utils::{chain::Chain, utils::test_wallets};
use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
use zksync_types::{H256, L1BatchNumber, via_wallet::SystemWalletsDetails};
use crate::btc_vote_inscription::{create_request, construct_voting_inscription_message};

async fn fixture() -> (ConnectionPool<Verifier>, MockBitcoinOps, bitcoin::Address, i64, Txid) {
    let pool = ConnectionPool::<Verifier>::test_pool().await;
    let chain = Chain::with_height(1);
    let source = MockBitcoinOps {
        headers: (0..=1)
            .map(|height| BlockHeaderInfo {
                height,
                hash: chain.blocks[&height].block_hash(),
                parent_hash: chain.blocks[&height].header.prev_blockhash,
                membership: HeaderMembership::Canonical(
                    std::num::NonZeroU32::new(2 - height).unwrap(),
                ),
            })
            .collect(),
        block: Some(chain.blocks[&1].clone()),
        block_height: 1,
        ..MockBitcoinOps::default()
    };
    let voter = get_mock_inscriber_and_conditions(source.clone()).inscriber_address().unwrap();
    let mut wallets = test_wallets();
    wallets.verifiers = vec![voter.clone()];
    let mut db = pool.connection().await.unwrap();
    db.via_store_mode_dal()
        .ensure_proof_verification_mode(&via_verifier_dal::via_store_mode_dal::StoreMode {
            proof_verification_dev_mode: false,
            bitcoin_network: "regtest".into(),
            bitcoin_genesis_hash: source.headers[0].hash.to_string(),
        })
        .await
        .unwrap();
    let mut tx = db.start_transaction().await.unwrap();
    assert!(via_test_utils::reader_at(&mut tx, &chain, 1, 1, bitcoin::Network::Regtest)
        .await
        .unwrap());
    let mut reader = ReaderDal::new(&mut tx);
    reader
        .insert_wallets(&SystemWalletsDetails::try_from(wallets).unwrap(), WalletSource::Unbound(0))
        .await
        .unwrap();
    let block = reader.header_at(1).await.unwrap().unwrap();
    let txid = Txid::from_byte_array(std::array::from_fn(|i| i as u8 + 1));
    let origin = MessageOrigin {
        height: 1,
        hash: block.header.block_hash,
        transaction_index: 0,
        message_index: 0,
        txid: Txid::from_byte_array([2; 32]),
    };
    let pubdata = via_test_utils::inscription::batch_reference(
        origin.clone(),
        L1BatchNumber(1),
        H256::repeat_byte(4),
        H256::zero(),
        "fixture",
        "batch",
    );
    let pubdata_txid = pubdata.origin.txid;
    let proof = via_test_utils::inscription::unsigned_proof(
        pubdata,
        MessageOrigin { txid, transaction_index: 1, ..origin },
        ProofDAReferenceInput {
            l1_batch_reveal_txid: pubdata_txid,
            da_identifier: "fixture".into(),
            blob_id: "proof".into(),
        },
    );
    tx.via_votes_dal().save_reader_proof(&proof, block.id).await.unwrap();
    let id = tx.via_votes_dal().pending_reader_proof().await.unwrap().unwrap().id;
    tx.commit().await.unwrap();
    (pool, source, voter, id, txid)
}

#[tokio::test]
async fn vote_request_waits_for_verified_canonical_proof_and_is_created_once() {
    let (pool, source, voter, id, txid) = fixture().await;
    create_request(&pool, &source, &voter).await.unwrap();
    let mut db = pool.connection().await.unwrap();
    assert!(db.via_btc_sender_dal().list_new_inscription_request(10).await.unwrap().is_empty());
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status=TRUE WHERE id=$1")
        .bind(id)
        .execute(db.conn())
        .await
        .unwrap();
    drop(db);
    create_request(&pool, &source, &voter).await.unwrap();
    create_request(&pool, &source, &voter).await.unwrap();
    let mut db = pool.connection().await.unwrap();
    let requests = db.via_btc_sender_dal().list_new_inscription_request(10).await.unwrap();
    assert_eq!(requests.len(), 1);
    let mut display = txid.to_byte_array().to_vec();
    display.reverse();
    assert_eq!(
        InscriptionMessage::from_bytes(requests[0].inscription_message.as_ref().unwrap()),
        construct_voting_inscription_message(true, display).unwrap()
    );
}

#[tokio::test]
async fn vote_request_refuses_incomplete_replay_and_source_outage() {
    let (pool, mut source, voter, id, _) = fixture().await;
    let mut db = pool.connection().await.unwrap();
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status=TRUE WHERE id=$1")
        .bind(id)
        .execute(db.conn())
        .await
        .unwrap();
    sqlx::query("UPDATE via_btc_reader SET replay_ready=FALSE").execute(db.conn()).await.unwrap();
    drop(db);
    assert!(create_request(&pool, &source, &voter).await.is_err());
    let mut db = pool.connection().await.unwrap();
    sqlx::query("UPDATE via_btc_reader SET replay_ready=TRUE").execute(db.conn()).await.unwrap();
    drop(db);
    source.headers.clear();
    assert!(create_request(&pool, &source, &voter).await.is_err());
    assert!(pool
        .connection()
        .await
        .unwrap()
        .via_btc_sender_dal()
        .list_new_inscription_request(10)
        .await
        .unwrap()
        .is_empty());
}

async fn finalize(pool: &ConnectionPool<Verifier>, source: &MockBitcoinOps) -> u64 {
    use via_btc_storage::{
        ReaderRole,
        transition::{ReaderSnapshot, PreparedTransition},
    };
    use via_btc_client::traits::BitcoinOps;
    let mut db = pool.connection().await.unwrap();
    let snapshot =
        ReaderSnapshot::load(&mut ReaderDal::new(&mut db), source.reorg_safe_depth().unwrap())
            .await
            .unwrap();
    let prepared = PreparedTransition::prepare_headers(source, snapshot).await.unwrap();
    prepared.revalidate(source).await.unwrap();
    let mut tx = Verifier::OWNER.begin(&mut db).await.unwrap();
    Verifier::lock_source(&mut tx).await.unwrap();
    let saved = ReaderDal::new(&mut tx).finalize_ready_proofs(&prepared, 0.66).await.unwrap();
    tx.commit().await.unwrap();
    saved
}

async fn save_vote(
    db: &mut via_verifier_dal::Connection<'_, Verifier>,
    vote: &Included<via_btc_client::types::ValidatorAttestationInput>,
    work: &via_btc_storage::ProofWork,
) {
    use via_btc_storage::ReaderRole;
    let mut tx = Verifier::OWNER.begin(db).await.unwrap();
    Verifier::lock_source(&mut tx).await.unwrap();
    ReaderDal::new(&mut tx).save_reader_vote(vote, work, bitcoin::Network::Regtest).await.unwrap();
    tx.commit().await.unwrap();
}

async fn verified_vote(
    approved: bool,
) -> (
    ConnectionPool<Verifier>,
    MockBitcoinOps,
    via_btc_storage::ProofWork,
    Included<via_btc_client::types::ValidatorAttestationInput>,
) {
    let (pool, source, voter, id, txid) = fixture().await;
    let mut db = pool.connection().await.unwrap();
    let work = db.via_votes_dal().pending_reader_proof().await.unwrap().unwrap();
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status=$2 WHERE id=$1")
        .bind(id)
        .bind(approved)
        .execute(db.conn())
        .await
        .unwrap();
    let vote = Included {
        origin: MessageOrigin {
            txid: Txid::from_byte_array([7; 32]),
            transaction_index: 2,
            ..work.proof.origin.clone()
        },
        payload: Payload {
            input: via_btc_client::types::ValidatorAttestationInput {
                reference_txid: txid,
                attestation: if approved {
                    via_btc_client::types::Vote::Ok
                } else {
                    via_btc_client::types::Vote::NotOk
                },
            },
            public_key: vec![],
            signature: vec![],
            sender_script: Some(voter.script_pubkey().into_bytes()),
        },
    };
    save_vote(&mut db, &vote, &work).await;
    (pool, source, work, vote)
}

#[tokio::test]
async fn finalization_captures_exact_proof_and_preserves_positive_and_negative_quorum() {
    for approved in [true, false] {
        let (pool, source, work, _) = verified_vote(approved).await;
        assert_eq!(finalize(&pool, &source).await, 1);
        assert_eq!(finalize(&pool, &source).await, 0);
        let row: (bool, i64) = sqlx::query_as(
            "SELECT is_finalized,finalized_header_id FROM via_votable_transactions WHERE id=$1",
        )
        .bind(work.id)
        .fetch_one(pool.connection().await.unwrap().conn())
        .await
        .unwrap();
        assert_eq!(row, (approved, work.header_id));
    }
}

#[tokio::test]
async fn finalization_waits_for_ready_mature_complete_canonical_evidence() {
    for (name,hold) in [
        ("replay","UPDATE via_btc_reader SET replay_ready=FALSE"),
        ("immature","UPDATE via_btc_reader SET confirmed_number=0,confirmed_hash=(SELECT hash FROM via_btc_headers WHERE number=0)"),
        ("local verdict","UPDATE via_votable_transactions SET l1_batch_status=NULL"),
        ("canonical incarnation","UPDATE via_votable_transactions SET proof_header_id=proof_header_id+100"),
        ("quorum","DELETE FROM via_votes"),
    ] {
        let (pool,source,_,_) = verified_vote(true).await;
        sqlx::query(hold).execute(pool.connection().await.unwrap().conn()).await.unwrap();
        assert_eq!(finalize(&pool,&source).await,0,"{name}");
        let receipt:Option<i64> = sqlx::query_scalar("SELECT finalized_header_id FROM via_votable_transactions").fetch_one(pool.connection().await.unwrap().conn()).await.unwrap();
        assert!(receipt.is_none(),"{name}");
    }
}

#[tokio::test]
async fn repeated_ballot_ignores_new_inclusion_and_witness_but_conflicting_vote_fails() {
    let (pool, _, work, mut vote) = verified_vote(true).await;
    let mut db = pool.connection().await.unwrap();
    let mut tx = db.start_transaction().await.unwrap();
    vote.payload.signature.push(1);
    vote.origin.message_index += 1;
    ReaderDal::new(&mut tx)
        .save_reader_vote(&vote, &work, bitcoin::Network::Regtest)
        .await
        .unwrap();
    let votes = ReaderDal::new(&mut tx).reader_votes(1, &[work.id]).await.unwrap();
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].height, 1);
    vote.payload.input.attestation = via_btc_client::types::Vote::NotOk;
    let error = ReaderDal::new(&mut tx)
        .save_reader_vote(&vote, &work, bitcoin::Network::Regtest)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<via_btc_client::indexer::replay::ReplayConflict>().unwrap().field,
        "vote"
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn retained_local_verdict_finalizes_only_after_canonical_vote_replacement() {
    let (pool, source, work, vote) = verified_vote(true).await;
    let mut db = pool.connection().await.unwrap();
    for query in [
        "UPDATE via_votable_transactions SET is_finalized=TRUE",
        "UPDATE via_votes SET source_l1_block_hash='orphan'",
    ] {
        sqlx::query(query).execute(db.conn()).await.unwrap();
    }
    assert_eq!(finalize(&pool, &source).await, 0);
    sqlx::query("DELETE FROM via_votes").execute(db.conn()).await.unwrap();
    save_vote(&mut db, &vote, &work).await;
    assert_eq!(finalize(&pool, &source).await, 1);
    let verdict: bool =
        sqlx::query_scalar("SELECT l1_batch_status FROM via_votable_transactions WHERE id=$1")
            .bind(work.id)
            .fetch_one(db.conn())
            .await
            .unwrap();
    assert!(verdict);
}

#[tokio::test]
async fn public_sender_run_publishes_and_completes_the_current_vote() {
    let (pool, mut source, _, id, _) = fixture().await;
    sqlx::query("UPDATE via_votable_transactions SET l1_batch_status=TRUE WHERE id=$1")
        .bind(id)
        .execute(pool.connection().await.unwrap().conn())
        .await
        .unwrap();
    for confirmed in [false, true] {
        source.tx_confirmation = confirmed;
        let mut config = super::utils::get_btc_sender_config(1, 1);
        config.poll_interval = 1;
        let manager =
            super::utils::get_inscription_manager_mock(pool.clone(), config, source.clone()).await;
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let run = tokio::spawn(manager.run(receiver));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let pending = pool
                    .connection()
                    .await
                    .unwrap()
                    .via_btc_sender_dal()
                    .get_inflight_inscriptions()
                    .await
                    .unwrap();
                if pending.is_empty() == confirmed {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn votes_after_the_frozen_prefix_cannot_supply_its_quorum() {
    let (pool, _, work, mut vote) = verified_vote(true).await;
    let mut db = pool.connection().await.unwrap();
    sqlx::query("DELETE FROM via_votes").execute(db.conn()).await.unwrap();
    let chain = Chain::with_height(2);
    let mut tx = db.start_transaction().await.unwrap();
    let mut reader = ReaderDal::new(&mut tx);
    let previous = reader.header_at(1).await.unwrap().unwrap();
    reader
        .save_block(
            Header {
                height: 2,
                block_hash: chain.blocks[&2].block_hash(),
                parent_hash: previous.header.block_hash,
            },
            &previous,
        )
        .await
        .unwrap();
    vote.origin.height = 2;
    vote.origin.hash = chain.blocks[&2].block_hash();
    reader.save_reader_vote(&vote, &work, bitcoin::Network::Regtest).await.unwrap();
    let votes = reader.reader_votes(2, &[work.id]).await.unwrap();
    assert!(matches!(
        reader.proof_quorum(&work, &votes, 1, 0.66).await.unwrap(),
        via_btc_storage::ProofQuorum::Complete(None)
    ));
    assert!(matches!(
        reader.proof_quorum(&work, &votes, 2, 0.66).await.unwrap(),
        via_btc_storage::ProofQuorum::Complete(Some(true))
    ));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn historical_membership_controls_old_proofs_after_rotation() {
    use via_btc_client::{indexer::configuration::ConfigurationInput, types::UpdateGovernanceInput};
    let (pool, _, work, mut vote) = verified_vote(true).await;
    let mut db = pool.connection().await.unwrap();
    let replacement = test_wallets().governance;
    let mut tx = db.start_transaction().await.unwrap();
    let mut reader = ReaderDal::new(&mut tx);
    let change = Included {
        origin: MessageOrigin { transaction_index: 3, ..work.proof.origin.clone() },
        payload: Payload {
            input: ConfigurationInput::Governance(UpdateGovernanceInput {
                inputs: vec![],
                address: replacement.as_unchecked().clone(),
            }),
            public_key: vec![],
            signature: vec![],
            sender_script: None,
        },
    };
    let id = reader.save_configuration(&change).await.unwrap();
    let mut details = SystemWalletsDetails::default();
    details.0.insert(
        zksync_types::via_wallet::WalletRole::Verifier,
        zksync_types::via_wallet::WalletInfo {
            addresses: vec![replacement.clone()],
            txid: change.origin.txid,
        },
    );
    reader.decide_configuration(id, true, &details).await.unwrap();
    assert_eq!(
        reader.wallets_before(2).await.unwrap().unwrap().verifiers,
        vec![replacement.clone()]
    );
    vote.origin.message_index = 1;
    vote.payload.sender_script = Some(replacement.script_pubkey().into_bytes());
    reader.save_reader_vote(&vote, &work, bitcoin::Network::Regtest).await.unwrap();
    let votes = reader.reader_votes(1, &[work.id]).await.unwrap();
    assert_eq!(votes.len(), 1);
    assert!(matches!(
        reader.proof_quorum(&work, &votes, 1, 0.66).await.unwrap(),
        via_btc_storage::ProofQuorum::Complete(Some(true))
    ));
}

#[tokio::test]
async fn only_pre_cutoff_finalized_history_is_exempt_from_rotation_evidence() {
    use via_btc_storage::{ReaderRole, ProofScope};
    let (pool, _, _, _, _) = fixture().await;
    let mut db = pool.connection().await.unwrap();
    sqlx::query("UPDATE via_votable_transactions SET is_finalized=TRUE,rotation_legacy_cutoff=1")
        .execute(db.conn())
        .await
        .unwrap();
    let proofs = Verifier::canonical_proofs(&mut db, 1, ProofScope::Interpretation).await.unwrap();
    assert!(proofs[0].rotation_exempt);
    assert_eq!(
        ReaderDal::new(&mut db).rotation_decision(&proofs, true, &[], 1, 0.66).await.unwrap(),
        via_reorg::configuration::RotationDecision::Accepted
    );
    sqlx::query("UPDATE via_votable_transactions SET rotation_legacy_cutoff=0")
        .execute(db.conn())
        .await
        .unwrap();
    let proofs = Verifier::canonical_proofs(&mut db, 1, ProofScope::Interpretation).await.unwrap();
    assert!(!proofs[0].rotation_exempt);
    assert_eq!(
        ReaderDal::new(&mut db).rotation_decision(&proofs, true, &[], 1, 0.66).await.unwrap(),
        via_reorg::configuration::RotationDecision::NotReady
    );
}
