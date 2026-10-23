//! Explicit SQL prerequisites for DAL tests; database lifecycle stays with each test's existing pool.
//! Adapted fixture composition from SQLx v0.8.2:
//! <https://github.com/launchbadge/sqlx/blob/10bec327703303eca5f8ec5809c4713a4294313d/tests/postgres/test-attr.rs>.

use bitcoin::{hashes::Hash, BlockHash, Txid};
use serde_json::{json, Value};
use sqlx::PgConnection;
use via_reorg::chain::{Header, MessageRef};

/// Synthetic canonical first source for DAL fixtures; real Core ancestry uses the chain fixture.
pub async fn first_source(
    connection: &mut PgConnection,
    seed: u8,
) -> sqlx::Result<Header<BlockHash>> {
    let source = Header {
        height: 1,
        block_hash: BlockHash::from_byte_array([seed; 32]),
        parent_hash: BlockHash::all_zeros(),
    };
    header(connection, source, true).await?;
    Ok(source)
}

pub async fn header(
    connection: &mut PgConnection,
    header: Header<BlockHash>,
    canonical: bool,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO via_btc_headers(hash,height,parent_hash) VALUES($1,$2,$3)
        ON CONFLICT(hash) DO UPDATE SET hash=CASE WHEN via_btc_headers.height=EXCLUDED.height
        AND via_btc_headers.parent_hash=EXCLUDED.parent_hash THEN via_btc_headers.hash ELSE NULL END")
        .bind(header.block_hash.as_byte_array().as_slice()).bind(i64::from(header.height))
        .bind(header.parent_hash.as_byte_array().as_slice()).execute(&mut *connection).await?;
    if canonical {
        sqlx::query("INSERT INTO via_l1_blocks(number,hash) VALUES($1,$2) ON CONFLICT(number) DO UPDATE
            SET hash=CASE WHEN via_l1_blocks.hash=EXCLUDED.hash THEN via_l1_blocks.hash ELSE NULL END")
            .bind(i64::from(header.height)).bind(header.block_hash.as_byte_array().as_slice()).execute(connection).await?;
    }
    Ok(())
}

pub async fn bootstrap_verifiers(
    connection: &mut PgConnection,
    members: &[String],
    baseline: i64,
) -> sqlx::Result<i64> {
    let addresses = members.join(",");
    let payload =
        json!({"role":"Verifier","addresses":addresses,"origin":"bootstrap","baseline":baseline});
    sqlx::query_scalar("INSERT INTO via_wallets(role,address,tx_hash,l1_block_number,origin_kind,immutable_payload)
        VALUES('Verifier',$1,$2,$3,'bootstrap',$4) ON CONFLICT(tx_hash,message_index,role) DO UPDATE
        SET immutable_payload=via_btc_immutable_payload(via_wallets.immutable_payload,EXCLUDED.immutable_payload) RETURNING id")
        .bind(addresses).bind(Txid::all_zeros().to_string()).bind(baseline).bind(payload).fetch_one(connection).await
}

/// The caller supplies a retained header and explicit historical set, rather than inferring either from height.
pub async fn proof(
    connection: &mut PgConnection,
    proof: Txid,
    batch: u32,
    payload: &Value,
    inclusion: MessageRef<BlockHash, Txid>,
    set_id: i64,
) -> sqlx::Result<i64> {
    let proof_id: i64 = sqlx::query_scalar(
        "INSERT INTO via_proof_evidence(proof_txid,l1_batch_number,immutable_payload)
        VALUES($1,$2,$3) RETURNING id",
    )
    .bind(proof.as_byte_array().as_slice())
    .bind(i64::from(batch))
    .bind(json!({"batch":batch,"message":payload}))
    .fetch_one(&mut *connection)
    .await?;
    sqlx::query_scalar("INSERT INTO via_proof_inclusions(proof_id,carrier_txid,block_hash,block_height,tx_index,message_index,verifier_set_id)
        VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id")
        .bind(proof_id).bind(inclusion.carrier_txid.as_byte_array().as_slice())
        .bind(inclusion.block.block_hash.as_byte_array().as_slice()).bind(i64::from(inclusion.block.height))
        .bind(i64::from(inclusion.tx_index)).bind(i64::from(inclusion.message_index)).bind(set_id)
        .fetch_one(connection).await
}

pub async fn vote(
    connection: &mut PgConnection,
    identity: Txid,
    proof: Txid,
    verifier: &str,
    approved: bool,
    inclusion: MessageRef<BlockHash, Txid>,
) -> sqlx::Result<i64> {
    let vote_id:i64=sqlx::query_scalar("INSERT INTO via_vote_evidence(txid,message_index,proof_txid,verifier_address,approved,immutable_payload)
        VALUES($1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(identity.as_byte_array().as_slice()).bind(i64::from(inclusion.message_index))
        .bind(proof.as_byte_array().as_slice()).bind(verifier).bind(approved)
        .bind(json!({"proof":proof.to_string(),"verifier":verifier,"approved":approved}))
        .fetch_one(&mut *connection).await?;
    sqlx::query_scalar("INSERT INTO via_vote_inclusions(vote_id,carrier_txid,block_hash,block_height,tx_index,message_index)
        VALUES($1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(vote_id).bind(inclusion.carrier_txid.as_byte_array().as_slice())
        .bind(inclusion.block.block_hash.as_byte_array().as_slice()).bind(i64::from(inclusion.block.height))
        .bind(i64::from(inclusion.tx_index)).bind(i64::from(inclusion.message_index)).fetch_one(connection).await
}

/// Explicit parsed-input fixture for tests of consumers other than the source writer.
#[derive(Debug, Clone)]
pub struct ProofVerdict {
    pub batch: u32,
    pub batch_hash: zksync_types::H256,
    pub parent_hash: zksync_types::H256,
    pub proof_txid: Txid,
    pub proof_blob: String,
    pub batch_txid: String,
    pub batch_blob: String,
    pub da_identifier: String,
}

pub async fn verdict(
    connection: &mut PgConnection,
    input: &ProofVerdict,
    inclusion: MessageRef<BlockHash, Txid>,
    set_id: i64,
) -> sqlx::Result<i64> {
    let payload = json!({"batch_hash":input.batch_hash,"parent_hash":input.parent_hash,
        "proof_blob":input.proof_blob,"batch_txid":input.batch_txid,"batch_blob":input.batch_blob,
        "da_identifier":input.da_identifier});
    let inclusion_id =
        proof(connection, input.proof_txid, input.batch, &payload, inclusion, set_id).await?;
    // The legacy protocol field keeps its existing display-byte representation; the carrier stays native.
    let mut display = input.proof_txid.to_byte_array();
    display.reverse();
    sqlx::query_scalar("INSERT INTO via_proof_verdicts(l1_batch_number,l1_batch_hash,prev_l1_batch_hash,
        proof_reveal_tx_id,proof_blob_id,pubdata_reveal_tx_id,pubdata_blob_id,da_identifier,
        source_l1_block_number,source_l1_block_hash,proof_l1_block_number,proof_l1_block_hash,reader_proof_inclusion_id)
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$9,$10,$11) RETURNING id")
        .bind(i64::from(input.batch)).bind(input.batch_hash.as_bytes()).bind(input.parent_hash.as_bytes())
        .bind(display.as_slice()).bind(&input.proof_blob).bind(&input.batch_txid).bind(&input.batch_blob)
        .bind(&input.da_identifier).bind(i64::from(inclusion.block.height))
        .bind(inclusion.block.block_hash.as_byte_array().as_slice()).bind(inclusion_id)
        .fetch_one(connection).await
}
