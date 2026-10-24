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

ALTER TABLE via_transactions ADD COLUMN bitcoin_height BIGINT, ADD COLUMN bitcoin_hash TEXT, ADD COLUMN bitcoin_vout BIGINT,
    ADD COLUMN bitcoin_payload JSONB;
CREATE INDEX ON via_transactions (l1_block_number, bitcoin_hash);
ALTER TABLE via_transactions DROP CONSTRAINT via_transactions_pkey;
CREATE UNIQUE INDEX via_transactions_outpoint ON via_transactions
    (tx_id, COALESCE(bitcoin_vout, priority_id & 65535));
ALTER TABLE protocol_versions ADD COLUMN bitcoin_height BIGINT, ADD COLUMN bitcoin_hash TEXT,
    ADD COLUMN bitcoin_payload JSONB;
ALTER TABLE protocol_patches ADD COLUMN bitcoin_height BIGINT, ADD COLUMN bitcoin_hash TEXT, ADD COLUMN bitcoin_payload JSONB;
ALTER TABLE via_votable_transactions ADD COLUMN proof_header_id BIGINT,
    ADD COLUMN bitcoin_payload JSONB, ADD COLUMN transaction_index INTEGER, ADD COLUMN message_index INTEGER;
ALTER TABLE via_votable_transactions DROP CONSTRAINT via_votable_transactions_l1_batch_hash_key,
    DROP CONSTRAINT via_votable_transactions_proof_blob_id_key,
    DROP CONSTRAINT via_votable_transactions_proof_reveal_tx_id_key,
    DROP CONSTRAINT via_votable_transactions_pubdata_blob_id_key,
    DROP CONSTRAINT via_votable_transactions_pubdata_reveal_tx_id_key;
DROP INDEX unique_canonical_batch;
CREATE UNIQUE INDEX via_votable_proof_inclusion ON via_votable_transactions
    (proof_reveal_tx_id, COALESCE(proof_l1_block_hash,''));
ALTER TABLE via_votes ADD COLUMN txid TEXT, ADD COLUMN transaction_index INTEGER,
    ADD COLUMN message_index INTEGER, ADD COLUMN bitcoin_payload JSONB;

-- Local sender/DA history must outlive reader suffixes and their retained parents.
ALTER TABLE via_btc_inscriptions_request_history DROP CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey,
    ADD CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey FOREIGN KEY (inscription_request_id) REFERENCES via_btc_inscriptions_request(id);
ALTER TABLE via_l1_batch_vote_inscription_request DROP CONSTRAINT via_l1_batch_vote_inscription_reque_votable_transaction_id_fkey,
    ADD CONSTRAINT via_l1_batch_vote_inscription_reque_votable_transaction_id_fkey FOREIGN KEY (votable_transaction_id) REFERENCES via_votable_transactions(id);
ALTER TABLE via_votes DROP CONSTRAINT via_votes_votable_transaction_id_fkey,
    ADD CONSTRAINT via_votes_votable_transaction_id_fkey FOREIGN KEY (votable_transaction_id) REFERENCES via_votable_transactions(id);
