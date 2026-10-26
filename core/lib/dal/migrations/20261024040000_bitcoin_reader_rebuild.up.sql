-- Own Via header schema, inspired by Midnight's block-table/position pattern; no DDL is copied.
-- https://github.com/midnightntwrk/midnight-indexer/blob/8b6ea73ea310d60519b3deb17b9f4d1c2342d9c9/indexer-common/migrations/postgres/001_initial.sql
CREATE TABLE via_btc_headers (
    id BIGSERIAL PRIMARY KEY,
    number BIGINT NOT NULL UNIQUE CHECK (number BETWEEN 0 AND 4294967295),
    hash TEXT NOT NULL UNIQUE,
    parent_hash TEXT NOT NULL
);
-- Hyperlane scraper_head confirmed frontier, adapted to one Bitcoin reader without leases or sparse checkpoints.
-- https://github.com/hyperlane-xyz/hyperlane-monorepo/blob/693d2d45df75135504e424c5a4d16761d0016f99/rust/main/migration/src/near_head.sql
CREATE TABLE via_btc_reader (
    singleton BOOLEAN PRIMARY KEY CHECK (singleton),
    format_version INTEGER NOT NULL,
    bitcoin_network TEXT NOT NULL,
    genesis_hash TEXT NOT NULL,
    bootstrap_number BIGINT NOT NULL,
    bootstrap_hash TEXT NOT NULL,
    replay_cutoff_number BIGINT NOT NULL,
    replay_cutoff_hash TEXT NOT NULL,
    confirmed_number BIGINT NOT NULL,
    confirmed_hash TEXT NOT NULL,
    replay_ready BOOLEAN NOT NULL DEFAULT FALSE,
    stopped_reason TEXT
);
CREATE TABLE via_btc_consumers (
    name TEXT PRIMARY KEY,
    header_id BIGINT NOT NULL REFERENCES via_btc_headers(id)
);
CREATE TABLE via_btc_configuration (
    id BIGSERIAL PRIMARY KEY,
    l1_block_number BIGINT NOT NULL,
    block_hash TEXT NOT NULL,
    transaction_index INTEGER NOT NULL,
    message_index INTEGER NOT NULL,
    txid TEXT NOT NULL,
    role TEXT NOT NULL,
    payload JSONB NOT NULL,
    accepted BOOLEAN,
    UNIQUE (block_hash, transaction_index, message_index, role)
);
CREATE INDEX ON via_btc_configuration (l1_block_number, transaction_index, message_index);
ALTER TABLE via_wallets ADD COLUMN block_hash TEXT, ADD COLUMN transaction_index INTEGER,
    ADD COLUMN message_index INTEGER;

ALTER TABLE transactions ADD COLUMN bitcoin_height BIGINT, ADD COLUMN bitcoin_hash TEXT, ADD COLUMN bitcoin_vout BIGINT,
    ADD COLUMN bitcoin_payload JSONB;
CREATE INDEX ON transactions (l1_block_number, bitcoin_hash) WHERE is_priority;
ALTER TABLE protocol_patches ADD COLUMN bitcoin_height BIGINT, ADD COLUMN bitcoin_hash TEXT,
    ADD COLUMN bitcoin_payload JSONB;
ALTER TABLE via_votes ADD COLUMN l1_block_number BIGINT, ADD COLUMN block_hash TEXT,
    ADD COLUMN txid TEXT, ADD COLUMN transaction_index INTEGER, ADD COLUMN message_index INTEGER,
    ADD COLUMN bitcoin_payload JSONB;
CREATE INDEX ON via_votes (l1_block_number, block_hash);
CREATE TABLE via_btc_proofs (
    id BIGSERIAL PRIMARY KEY,
    l1_batch_number BIGINT NOT NULL,
    proof_reveal_tx_id BYTEA NOT NULL,
    pubdata_reveal_tx_id BYTEA NOT NULL,
    l1_block_number BIGINT NOT NULL,
    block_hash TEXT NOT NULL,
    transaction_index INTEGER NOT NULL,
    message_index INTEGER NOT NULL,
    payload JSONB NOT NULL,
    UNIQUE (proof_reveal_tx_id, block_hash)
);
CREATE INDEX ON via_btc_proofs (l1_block_number, transaction_index, message_index);
CREATE TABLE via_btc_finalizations (
    proof_id BIGINT PRIMARY KEY REFERENCES via_btc_proofs(id),
    approved BOOLEAN NOT NULL
);
