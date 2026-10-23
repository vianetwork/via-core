-- Hash conversion is supplied by the stopped Rust migrator, never by SQL hex reversal.
DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM via_l1_blocks) AND to_regclass('pg_temp.via_btc_legacy_hashes') IS NULL THEN
        RAISE EXCEPTION 'stopped Bitcoin semantic migration is required before applying this schema';
    END IF;
END $$;
CREATE FUNCTION pg_temp.via_btc_checked_legacy_hash(display_hash TEXT) RETURNS BYTEA
LANGUAGE plpgsql STRICT AS $$
DECLARE checked BYTEA;
BEGIN
    SELECT hash INTO STRICT checked FROM pg_temp.via_btc_legacy_hashes WHERE display=display_hash;
    IF octet_length(checked) <> 32 THEN RAISE EXCEPTION 'invalid checked Bitcoin hash'; END IF;
    RETURN checked;
END;
$$;
ALTER TABLE via_l1_blocks ALTER COLUMN hash TYPE BYTEA USING pg_temp.via_btc_checked_legacy_hash(hash);
ALTER TABLE via_l1_blocks ADD FOREIGN KEY(number,hash) REFERENCES via_btc_headers(height,hash);
ALTER TABLE via_l1_blocks ADD CHECK(octet_length(hash)=32);

ALTER TABLE via_votable_transactions ALTER COLUMN source_l1_block_hash TYPE BYTEA USING pg_temp.via_btc_checked_legacy_hash(source_l1_block_hash);
ALTER TABLE via_votable_transactions ALTER COLUMN proof_l1_block_hash TYPE BYTEA USING pg_temp.via_btc_checked_legacy_hash(proof_l1_block_hash);
ALTER TABLE via_votes ALTER COLUMN source_l1_block_hash TYPE BYTEA USING pg_temp.via_btc_checked_legacy_hash(source_l1_block_hash);
-- Withdrawal inclusions already retain checked bytes; remove the redundant writable display hash.
ALTER TABLE via_withdrawal_inclusions DROP COLUMN block_hash_text;

-- Semantic migration runs with every database writer stopped. Legacy origins remain unproved.
-- Idea: fixed-order immutable replay checks, TigerBeetle 0450c32b2c3c3ab6114f3402cc601341cc8192aa,
-- src/state_machine.zig:3971-4033. No transfer-specific exceptions apply to Via evidence.
CREATE FUNCTION via_btc_immutable_payload(previous JSONB, incoming JSONB, field_path TEXT DEFAULT '')
RETURNS JSONB LANGUAGE plpgsql AS $$
DECLARE field_name TEXT;
BEGIN
    IF previous IS NOT DISTINCT FROM incoming THEN RETURN previous; END IF;
    IF jsonb_typeof(previous) = 'object' AND jsonb_typeof(incoming) = 'object' THEN
        FOR field_name IN
            SELECT key FROM jsonb_object_keys(previous || incoming) AS key ORDER BY key
        LOOP
            PERFORM via_btc_immutable_payload(previous -> field_name, incoming -> field_name,
                CASE WHEN field_path = '' THEN field_name ELSE field_path || '_' || field_name END);
        END LOOP;
    END IF;
    RAISE EXCEPTION 'exists_with_different_%', CASE WHEN field_path = '' THEN 'payload' ELSE field_path END
        USING ERRCODE = '23514';
END;
$$;

ALTER TABLE via_wallets ADD COLUMN origin_kind TEXT NOT NULL DEFAULT 'legacy'
    CHECK (origin_kind IN ('legacy', 'bootstrap', 'bitcoin'));
ALTER TABLE via_wallets ADD COLUMN message_index BIGINT NOT NULL DEFAULT 0 CHECK (message_index >= 0);
ALTER TABLE via_wallets ADD COLUMN immutable_payload JSONB;
ALTER TABLE via_wallets ADD CONSTRAINT via_wallet_identity UNIQUE (tx_hash, message_index, role);
CREATE TABLE via_wallet_inclusions (
    id BIGSERIAL PRIMARY KEY,
    wallet_id BIGINT NOT NULL REFERENCES via_wallets(id),
    carrier_txid BYTEA NOT NULL CHECK(octet_length(carrier_txid)=32),
    block_hash BYTEA NOT NULL CHECK (octet_length(block_hash) = 32),
    block_height BIGINT NOT NULL CHECK (block_height BETWEEN 0 AND 4294967295),
    tx_index BIGINT NOT NULL CHECK (tx_index BETWEEN 0 AND 4294967295),
    message_index BIGINT NOT NULL CHECK (message_index BETWEEN 0 AND 4294967295),
    decision TEXT NOT NULL CHECK (decision IN ('accepted', 'duplicate_role', 'earlier_unfinalized_proof', 'bad_update')),
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY (block_height, block_hash) REFERENCES via_btc_headers(height, hash)
);
CREATE INDEX via_wallet_inclusions_order ON via_wallet_inclusions(block_height, tx_index, message_index);

CREATE TABLE via_deposit_evidence (
    txid BYTEA NOT NULL CHECK (octet_length(txid) = 32),
    vout BIGINT NOT NULL CHECK (vout BETWEEN 0 AND 4294967295),
    immutable_payload JSONB NOT NULL CHECK (jsonb_typeof(immutable_payload) = 'object'),
    PRIMARY KEY (txid, vout)
);
CREATE TABLE via_deposit_inclusions (
    id BIGSERIAL PRIMARY KEY,
    txid BYTEA NOT NULL,
    vout BIGINT NOT NULL,
    carrier_txid BYTEA NOT NULL CHECK (octet_length(carrier_txid) = 32),
    block_hash BYTEA NOT NULL CHECK (octet_length(block_hash) = 32),
    block_height BIGINT NOT NULL CHECK (block_height BETWEEN 0 AND 4294967295),
    tx_index BIGINT NOT NULL CHECK (tx_index BETWEEN 0 AND 4294967295),
    message_index BIGINT NOT NULL CHECK (message_index BETWEEN 0 AND 4294967295),
    operation_hash BYTEA NOT NULL CHECK (octet_length(operation_hash) = 32),
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY (txid, vout) REFERENCES via_deposit_evidence(txid, vout),
    FOREIGN KEY (block_height, block_hash) REFERENCES via_btc_headers(height, hash)
);

CREATE TABLE via_proof_evidence (
    id BIGSERIAL PRIMARY KEY,
    proof_txid BYTEA UNIQUE NOT NULL CHECK (octet_length(proof_txid) = 32),
    l1_batch_number BIGINT NOT NULL,
    immutable_payload JSONB NOT NULL CHECK (jsonb_typeof(immutable_payload) = 'object')
);
CREATE TABLE via_proof_inclusions (
    id BIGSERIAL PRIMARY KEY,
    proof_id BIGINT NOT NULL REFERENCES via_proof_evidence(id),
    carrier_txid BYTEA NOT NULL CHECK (octet_length(carrier_txid) = 32),
    block_hash BYTEA NOT NULL CHECK (octet_length(block_hash) = 32),
    block_height BIGINT NOT NULL CHECK (block_height BETWEEN 0 AND 4294967295),
    tx_index BIGINT NOT NULL CHECK (tx_index BETWEEN 0 AND 4294967295),
    message_index BIGINT NOT NULL CHECK (message_index BETWEEN 0 AND 4294967295),
    verifier_set_id BIGINT NOT NULL REFERENCES via_wallets(id),
    revoked_at TIMESTAMPTZ,
    UNIQUE (id, verifier_set_id),
    FOREIGN KEY (block_height, block_hash) REFERENCES via_btc_headers(height, hash)
);
CREATE TABLE via_proof_finalization (
    proof_inclusion_id BIGINT PRIMARY KEY,
    verifier_set_id BIGINT NOT NULL,
    decision BOOLEAN,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY (proof_inclusion_id, verifier_set_id) REFERENCES via_proof_inclusions(id, verifier_set_id)
);
CREATE TABLE via_vote_evidence (
    id BIGSERIAL PRIMARY KEY,
    txid BYTEA NOT NULL CHECK (octet_length(txid) = 32),
    message_index BIGINT NOT NULL CHECK (message_index BETWEEN 0 AND 4294967295),
    proof_txid BYTEA NOT NULL CHECK (octet_length(proof_txid) = 32),
    verifier_address TEXT NOT NULL,
    approved BOOLEAN NOT NULL,
    immutable_payload JSONB NOT NULL CHECK (jsonb_typeof(immutable_payload) = 'object'),
    UNIQUE (txid, message_index)
);
CREATE TABLE via_vote_inclusions (
    id BIGSERIAL PRIMARY KEY,
    vote_id BIGINT NOT NULL REFERENCES via_vote_evidence(id),
    carrier_txid BYTEA NOT NULL CHECK (octet_length(carrier_txid) = 32),
    block_hash BYTEA NOT NULL CHECK (octet_length(block_hash) = 32),
    block_height BIGINT NOT NULL CHECK (block_height BETWEEN 0 AND 4294967295),
    tx_index BIGINT NOT NULL CHECK (tx_index BETWEEN 0 AND 4294967295),
    message_index BIGINT NOT NULL CHECK (message_index BETWEEN 0 AND 4294967295),
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY (block_height, block_hash) REFERENCES via_btc_headers(height, hash)
);
CREATE TABLE via_upgrade_evidence (
    id BIGSERIAL PRIMARY KEY,
    txid BYTEA NOT NULL CHECK (octet_length(txid) = 32),
    message_index BIGINT NOT NULL CHECK (message_index BETWEEN 0 AND 4294967295),
    minor INTEGER NOT NULL,
    patch BIGINT NOT NULL,
    immutable_payload JSONB NOT NULL CHECK (jsonb_typeof(immutable_payload) = 'object'),
    UNIQUE (txid, message_index)
);
CREATE TABLE via_upgrade_inclusions (
    id BIGSERIAL PRIMARY KEY,
    upgrade_id BIGINT NOT NULL REFERENCES via_upgrade_evidence(id),
    carrier_txid BYTEA NOT NULL CHECK(octet_length(carrier_txid)=32),
    block_hash BYTEA NOT NULL CHECK (octet_length(block_hash) = 32),
    block_height BIGINT NOT NULL CHECK (block_height BETWEEN 0 AND 4294967295),
    tx_index BIGINT NOT NULL CHECK (tx_index BETWEEN 0 AND 4294967295),
    message_index BIGINT NOT NULL CHECK (message_index BETWEEN 0 AND 4294967295),
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY (block_height, block_hash) REFERENCES via_btc_headers(height, hash)
);

ALTER TABLE via_votable_transactions ADD COLUMN reader_proof_inclusion_id BIGINT REFERENCES via_proof_inclusions(id);

ALTER TABLE protocol_patches ADD COLUMN immutable_payload JSONB;

ALTER TABLE via_transactions DROP CONSTRAINT via_transactions_pkey;
ALTER TABLE via_transactions ADD PRIMARY KEY(canonical_tx_hash);

ALTER TABLE via_proof_inclusions ADD COLUMN l2_eligible BOOLEAN NOT NULL DEFAULT TRUE;
-- Retain the source of replacement so undo can expose the preceding admitted version.
-- https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/relational.rs
CREATE TABLE via_proof_l2_exclusions (
    excluded_inclusion_id BIGINT NOT NULL REFERENCES via_proof_inclusions(id),
    excluding_inclusion_id BIGINT NOT NULL REFERENCES via_proof_inclusions(id),
    PRIMARY KEY(excluded_inclusion_id,excluding_inclusion_id),
    CHECK(excluded_inclusion_id<>excluding_inclusion_id)
);
CREATE VIEW via_current_proof_inclusions AS
    SELECT inclusion.* FROM via_proof_inclusions inclusion
    JOIN via_l1_blocks canonical ON canonical.number=inclusion.block_height AND canonical.hash=inclusion.block_hash
    WHERE inclusion.revoked_at IS NULL AND inclusion.l2_eligible
    AND NOT EXISTS(SELECT 1 FROM via_proof_l2_exclusions exclusion
        JOIN via_proof_inclusions cause ON cause.id=exclusion.excluding_inclusion_id
        JOIN via_l1_blocks cause_block ON cause_block.number=cause.block_height AND cause_block.hash=cause.block_hash
        WHERE exclusion.excluded_inclusion_id=inclusion.id AND cause.revoked_at IS NULL);
DROP INDEX unique_canonical_batch;
ALTER TABLE via_votable_transactions RENAME TO via_proof_verdicts;
ALTER TABLE via_proof_verdicts ADD UNIQUE(reader_proof_inclusion_id);
ALTER TABLE via_proof_verdicts DROP CONSTRAINT via_votable_transactions_l1_batch_hash_key;
ALTER TABLE via_proof_verdicts DROP CONSTRAINT via_votable_transactions_proof_blob_id_key;
ALTER TABLE via_proof_verdicts DROP CONSTRAINT via_votable_transactions_proof_reveal_tx_id_key;
ALTER TABLE via_proof_verdicts DROP CONSTRAINT via_votable_transactions_pubdata_blob_id_key;
ALTER TABLE via_proof_verdicts DROP CONSTRAINT via_votable_transactions_pubdata_reveal_tx_id_key;
CREATE VIEW via_votable_transactions AS
    SELECT v.id,v.l1_batch_number,v.l1_batch_hash,v.prev_l1_batch_hash,v.proof_blob_id,v.proof_reveal_tx_id,
        v.pubdata_blob_id,v.pubdata_reveal_tx_id,v.da_identifier,
        f.decision AS is_finalized,
        v.l1_batch_status,v.created_at,v.updated_at,v.source_l1_block_number,v.source_l1_block_hash,
        v.proof_l1_block_number,v.proof_l1_block_hash,v.unverified_dev,v.reader_proof_inclusion_id
    FROM via_proof_verdicts v
    JOIN via_current_proof_inclusions i ON i.id=v.reader_proof_inclusion_id
    LEFT JOIN via_proof_finalization f ON f.proof_inclusion_id=i.id
    WHERE NOT EXISTS(SELECT 1 FROM via_l1_reorg);

ALTER TABLE via_btc_inscriptions_request_history DROP CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey;
ALTER TABLE via_btc_inscriptions_request_history ADD FOREIGN KEY(inscription_request_id) REFERENCES via_btc_inscriptions_request(id) ON DELETE RESTRICT;
ALTER TABLE via_l1_batch_vote_inscription_request DROP CONSTRAINT via_l1_batch_vote_inscription_reque_votable_transaction_id_fkey;
ALTER TABLE via_l1_batch_vote_inscription_request ADD FOREIGN KEY(votable_transaction_id) REFERENCES via_proof_verdicts(id) ON DELETE RESTRICT;
ALTER TABLE via_votes DROP CONSTRAINT via_votes_votable_transaction_id_fkey;
ALTER TABLE via_votes ADD FOREIGN KEY(votable_transaction_id) REFERENCES via_proof_verdicts(id) ON DELETE RESTRICT;

ALTER TABLE via_votes RENAME TO via_legacy_votes;

CREATE UNIQUE INDEX via_wallet_inclusions_active_identity ON via_wallet_inclusions(wallet_id, block_hash, tx_index, message_index) WHERE revoked_at IS NULL;

CREATE UNIQUE INDEX via_deposit_inclusions_active_identity ON via_deposit_inclusions(txid, vout, block_hash, tx_index, message_index) WHERE revoked_at IS NULL;

CREATE UNIQUE INDEX via_proof_inclusions_active_identity ON via_proof_inclusions(proof_id, block_hash, tx_index, message_index) WHERE revoked_at IS NULL;

CREATE UNIQUE INDEX via_vote_inclusions_active_identity ON via_vote_inclusions(vote_id, block_hash, tx_index, message_index) WHERE revoked_at IS NULL;

CREATE UNIQUE INDEX via_upgrade_inclusions_active_identity ON via_upgrade_inclusions(upgrade_id, block_hash, tx_index, message_index) WHERE revoked_at IS NULL;

-- R1 created no authority positions or certificates. Never reinterpret a preexisting active identity.
DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM via_btc_scan_positions) OR EXISTS(SELECT 1 FROM via_btc_migration_certificates) THEN
        RAISE EXCEPTION 'R1 certificate identities must remain inert before semantic migration';
    END IF;
END $$;
ALTER TABLE via_btc_scan_positions DROP CONSTRAINT via_btc_scan_positions_certificate_id_group_name_fkey;
ALTER TABLE via_btc_migration_certificates ALTER COLUMN id DROP DEFAULT;
ALTER TABLE via_btc_migration_certificates ALTER COLUMN id TYPE UUID USING NULL::UUID;
ALTER TABLE via_btc_migration_certificates ALTER COLUMN id SET DEFAULT gen_random_uuid();
DROP SEQUENCE via_btc_migration_certificates_id_seq;
ALTER TABLE via_btc_scan_positions ALTER COLUMN certificate_id TYPE UUID USING NULL::UUID;
ALTER TABLE via_btc_scan_positions ADD FOREIGN KEY(certificate_id,group_name) REFERENCES via_btc_migration_certificates(id,scope);
ALTER TABLE via_btc_migration_certificates ADD CHECK(octet_length(body::TEXT)<=134217728);
ALTER TABLE via_btc_migration_certificates ADD COLUMN application_status TEXT NOT NULL DEFAULT 'planned'
    CHECK(application_status IN ('planned','applied'));
ALTER TABLE via_btc_migration_certificates ADD COLUMN applied_at TIMESTAMPTZ;
ALTER TABLE via_btc_migration_certificates ADD CHECK((application_status='applied')=(applied_at IS NOT NULL));

ALTER TABLE via_proof_verdicts ADD COLUMN completed_input_identity JSONB CHECK(jsonb_typeof(completed_input_identity)='object');

ALTER TABLE via_transactions ADD UNIQUE(priority_id);

ALTER TABLE protocol_patches ADD COLUMN via_bootstrap BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE protocol_patches ADD COLUMN upgrade_tx_hash BYTEA CHECK(octet_length(upgrade_tx_hash)=32);
ALTER TABLE protocol_patches ADD COLUMN executed BOOLEAN NOT NULL DEFAULT FALSE;

-- An orphaned vote request stays retained but cannot authorize another external action.
CREATE FUNCTION via_btc_request_is_eligible(request_id BIGINT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
    SELECT NOT EXISTS(SELECT 1 FROM via_l1_reorg) AND COUNT(*)=1
    FROM via_l1_batch_vote_inscription_request request
    JOIN via_votable_transactions current ON current.id=request.votable_transaction_id
    WHERE request.vote_l1_batch_inscription_id=request_id AND current.l1_batch_status IS NOT NULL;
$$;
