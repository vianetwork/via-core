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

ALTER TABLE protocol_patches ADD COLUMN immutable_payload JSONB;

-- Requests and signed history are delivery facts; recovery cannot cascade through their cycle.
ALTER TABLE via_btc_inscriptions_request DROP CONSTRAINT via_btc_inscriptions_request_confirmed_inscriptions_reques_fkey;
ALTER TABLE via_btc_inscriptions_request ADD FOREIGN KEY(confirmed_inscriptions_request_history_id) REFERENCES via_btc_inscriptions_request_history(id) ON DELETE RESTRICT;
ALTER TABLE via_btc_inscriptions_request DROP CONSTRAINT via_btc_inscriptions_request_l1_batch_number_fkey;
ALTER TABLE via_btc_inscriptions_request_history DROP CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey;
ALTER TABLE via_btc_inscriptions_request_history ADD FOREIGN KEY(inscription_request_id) REFERENCES via_btc_inscriptions_request(id) ON DELETE RESTRICT;
ALTER TABLE via_l1_batch_inscription_request DROP CONSTRAINT via_l1_batch_inscription_request_l1_batch_number_fkey;
ALTER TABLE via_l1_batch_inscription_request DROP CONSTRAINT via_l1_batch_inscription_requ_commit_l1_batch_inscription__fkey;
ALTER TABLE via_l1_batch_inscription_request ADD FOREIGN KEY(commit_l1_batch_inscription_id) REFERENCES via_btc_inscriptions_request(id) ON DELETE RESTRICT;
ALTER TABLE via_l1_batch_inscription_request DROP CONSTRAINT via_l1_batch_inscription_reque_commit_proof_inscription_id_fkey;
ALTER TABLE via_l1_batch_inscription_request ADD FOREIGN KEY(commit_proof_inscription_id) REFERENCES via_btc_inscriptions_request(id) ON DELETE RESTRICT;
ALTER TABLE via_l1_batch_inscription_request RENAME TO via_batch_delivery_facts;
ALTER TABLE via_batch_delivery_facts ADD COLUMN source_batch_hash BYTEA CHECK(octet_length(source_batch_hash)=32);
ALTER TABLE via_batch_delivery_facts DROP CONSTRAINT via_l1_batch_inscription_request_l1_batch_number_key;
ALTER TABLE via_batch_delivery_facts ADD UNIQUE(l1_batch_number,source_batch_hash);
CREATE VIEW via_l1_batch_inscription_request AS
    SELECT d.l1_batch_number,d.commit_l1_batch_inscription_id,d.commit_proof_inscription_id,
        CASE WHEN authority.candidates=1 THEN authority.decisions[1] END AS is_finalized,
        d.created_at,GREATEST(d.updated_at,authority.updated_at) AS updated_at
    FROM via_batch_delivery_facts d JOIN l1_batches l2 ON l2.number=d.l1_batch_number AND l2.hash=d.source_batch_hash
    LEFT JOIN LATERAL (
        SELECT COUNT(*) AS candidates,array_agg(f.decision) AS decisions,MAX(f.updated_at) AS updated_at
        FROM via_proof_inclusions i JOIN via_proof_evidence p ON p.id=i.proof_id
        JOIN via_l1_blocks b ON b.number=i.block_height AND b.hash=i.block_hash
        JOIN via_btc_inscriptions_request_history h ON h.reveal_tx_id=i.carrier_txid
            AND h.inscription_request_id=d.commit_proof_inscription_id
        LEFT JOIN via_proof_finalization f ON f.proof_inclusion_id=i.id
        WHERE i.revoked_at IS NULL AND p.l1_batch_number=d.l1_batch_number
            AND p.immutable_payload #>> '{message,batch,input,l1_batch_hash}'='0x'||encode(l2.hash,'hex')
            AND NOT EXISTS(SELECT 1 FROM via_l1_reorg)
    ) authority ON TRUE;
ALTER TABLE via_votes DROP CONSTRAINT via_votes_l1_batch_number_fkey;
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

CREATE UNIQUE INDEX via_priority_operation_identity ON transactions(priority_op_id) WHERE is_priority AND priority_op_id IS NOT NULL;

ALTER TABLE protocol_patches ADD COLUMN via_bootstrap BOOLEAN NOT NULL DEFAULT FALSE;

-- Sender history retains physical risk; eligibility follows the exact current L2 subject.
CREATE FUNCTION via_btc_request_is_eligible(request_id BIGINT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
    SELECT NOT EXISTS(SELECT 1 FROM via_l1_reorg) AND COUNT(*)=1
    FROM via_batch_delivery_facts d JOIN l1_batches b ON b.number=d.l1_batch_number AND b.hash=d.source_batch_hash
    WHERE d.commit_l1_batch_inscription_id=request_id OR d.commit_proof_inscription_id=request_id;
$$;
