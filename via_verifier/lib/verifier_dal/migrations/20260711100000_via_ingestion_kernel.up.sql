-- Ingestion kernel observation store and projections (shadow tables).
-- These tables are written only by the kernel's aggregate adapter; the
-- legacy watcher tables are untouched until the shadow comparison passes.

CREATE TABLE via_ingestion_checkpoint (
    -- Single-row table: the row id is always TRUE.
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    height BIGINT NOT NULL,
    block_hash BYTEA NOT NULL,
    kernel_version INT NOT NULL,
    observation_rule_version INT NOT NULL,
    context_hash BYTEA NOT NULL,
    last_plan_hash BYTEA NOT NULL,
    context_blob JSONB NOT NULL,
    canonical_revision BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One row per applied canonical block; the restore source for reverts.
CREATE TABLE via_ingestion_chain (
    height BIGINT PRIMARY KEY,
    block_hash BYTEA NOT NULL UNIQUE,
    prev_hash BYTEA NOT NULL,
    header_time BIGINT NOT NULL,
    plan_hash BYTEA NOT NULL,
    context_blob JSONB NOT NULL
);

-- Immutable raw transaction bytes, keyed by both identities. Never deleted
-- on revert (content-addressed cache).
CREATE TABLE via_ingestion_raw_variants (
    txid BYTEA NOT NULL,
    wtxid BYTEA NOT NULL,
    raw BYTEA NOT NULL,
    PRIMARY KEY (txid, wtxid)
);

-- Placement of a transaction in a block. Orphaned placements flip
-- canonical to FALSE on revert; they are never deleted.
CREATE TABLE via_ingestion_inclusions (
    block_hash BYTEA NOT NULL,
    height BIGINT NOT NULL,
    tx_index BIGINT NOT NULL,
    txid BYTEA NOT NULL,
    wtxid BYTEA NOT NULL,
    canonical BOOLEAN NOT NULL,
    PRIMARY KEY (block_hash, tx_index)
);
CREATE INDEX via_ingestion_inclusions_txid ON via_ingestion_inclusions (txid);

CREATE TABLE via_ingestion_tracked_outputs (
    txid BYTEA NOT NULL,
    vout BIGINT NOT NULL,
    value_sat BIGINT NOT NULL,
    script BYTEA NOT NULL,
    role SMALLINT NOT NULL,
    canonical_spender_txid BYTEA,
    created_block_hash BYTEA NOT NULL,
    PRIMARY KEY (txid, vout)
);
CREATE INDEX via_ingestion_tracked_outputs_block ON via_ingestion_tracked_outputs (created_block_hash);

-- Every spend observation on any branch; canonicity is derived from the
-- spending block's inclusion row.
CREATE TABLE via_ingestion_tracked_spends (
    txid BYTEA NOT NULL,
    vout BIGINT NOT NULL,
    spending_txid BYTEA NOT NULL,
    spending_wtxid BYTEA NOT NULL,
    input_index BIGINT NOT NULL,
    block_hash BYTEA NOT NULL,
    PRIMARY KEY (txid, vout, spending_txid, block_hash)
);
CREATE INDEX via_ingestion_tracked_spends_block ON via_ingestion_tracked_spends (block_hash);

CREATE TABLE via_ingestion_deposits (
    block_hash BYTEA NOT NULL,
    height BIGINT NOT NULL,
    header_time BIGINT NOT NULL,
    ordinal_tx_index BIGINT NOT NULL,
    ordinal_location_tag SMALLINT NOT NULL,
    ordinal_location_index BIGINT NOT NULL,
    subject_txid BYTEA NOT NULL,
    subject_vout BIGINT NOT NULL,
    amount_sat BIGINT NOT NULL,
    receiver BYTEA NOT NULL,
    l2_contract BYTEA NOT NULL,
    call_data BYTEA NOT NULL,
    sender_script BYTEA,
    encoding SMALLINT NOT NULL,
    source_wtxid BYTEA NOT NULL,
    PRIMARY KEY (subject_txid, subject_vout, block_hash)
);
CREATE INDEX via_ingestion_deposits_block ON via_ingestion_deposits (block_hash);

CREATE TABLE via_ingestion_batch_refs (
    block_hash BYTEA NOT NULL,
    subject_txid BYTEA NOT NULL,
    l1_batch_index BIGINT NOT NULL,
    l1_batch_hash BYTEA NOT NULL,
    prev_l1_batch_hash BYTEA NOT NULL,
    da_identifier TEXT NOT NULL,
    blob_id TEXT NOT NULL,
    PRIMARY KEY (subject_txid, block_hash)
);
CREATE INDEX via_ingestion_batch_refs_block ON via_ingestion_batch_refs (block_hash);

CREATE TABLE via_ingestion_proof_refs (
    block_hash BYTEA NOT NULL,
    subject_txid BYTEA NOT NULL,
    da_identifier TEXT NOT NULL,
    blob_id TEXT NOT NULL,
    batch_reveal_txid BYTEA NOT NULL,
    l1_batch_index BIGINT NOT NULL,
    l1_batch_hash BYTEA NOT NULL,
    PRIMARY KEY (subject_txid, block_hash)
);
CREATE INDEX via_ingestion_proof_refs_block ON via_ingestion_proof_refs (block_hash);

CREATE TABLE via_ingestion_votes (
    block_hash BYTEA NOT NULL,
    subject_txid BYTEA NOT NULL,
    reference_txid BYTEA NOT NULL,
    attester_script BYTEA NOT NULL,
    ok BOOLEAN NOT NULL,
    l1_batch_index BIGINT NOT NULL,
    PRIMARY KEY (subject_txid, block_hash)
);
CREATE INDEX via_ingestion_votes_block ON via_ingestion_votes (block_hash);

CREATE TABLE via_ingestion_withdrawals (
    block_hash BYTEA NOT NULL,
    subject_txid BYTEA NOT NULL,
    withdrawal_index BIGINT NOT NULL,
    l2_id BYTEA NOT NULL,
    l2_tx_event_index INT NOT NULL,
    receiver_script BYTEA NOT NULL,
    amount_sat BIGINT NOT NULL,
    PRIMARY KEY (subject_txid, withdrawal_index, block_hash)
);
CREATE INDEX via_ingestion_withdrawals_block ON via_ingestion_withdrawals (block_hash);

-- Wallet history and applied protocol versions (from bootstrap, rotations,
-- and upgrade activations).
CREATE TABLE via_ingestion_wallet_history (
    block_hash BYTEA NOT NULL,
    subject_txid BYTEA NOT NULL,
    role SMALLINT NOT NULL,
    script BYTEA NOT NULL,
    verifier_position BIGINT,
    PRIMARY KEY (block_hash, subject_txid, role, script)
);

CREATE TABLE via_ingestion_protocol_versions (
    block_hash BYTEA NOT NULL,
    version_minor BIGINT NOT NULL,
    version_patch BIGINT NOT NULL,
    PRIMARY KEY (block_hash, version_minor, version_patch)
);

CREATE TABLE via_ingestion_rejections (
    block_hash BYTEA NOT NULL,
    ordinal_tx_index BIGINT NOT NULL,
    ordinal_location_tag SMALLINT NOT NULL,
    ordinal_location_index BIGINT NOT NULL,
    code SMALLINT NOT NULL,
    PRIMARY KEY (block_hash, ordinal_tx_index, ordinal_location_tag, ordinal_location_index)
);

-- Effects consumed downstream (sealed into an L2 batch, attested). The
-- reorg safety frontier reads this table.
CREATE TABLE via_ingestion_consumed_effects (
    block_hash BYTEA NOT NULL,
    effect_key BYTEA NOT NULL,
    PRIMARY KEY (block_hash, effect_key)
);

-- Durable hard-reorg halt; at most one row. While present, apply_block
-- fails and only explicit operator recovery clears it.
CREATE TABLE via_ingestion_hard_reorg_halt (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    halt_blob JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
