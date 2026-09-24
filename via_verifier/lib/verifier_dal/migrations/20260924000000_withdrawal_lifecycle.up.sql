-- A source tip covers the proof and every retained vote. NULL means incomplete
-- provenance and is never withdrawal authority; legacy rows are not backfilled.
ALTER TABLE via_votable_transactions
    ADD COLUMN source_l1_block_number BIGINT,
    ADD COLUMN source_l1_block_hash TEXT,
    ADD COLUMN proof_l1_block_number BIGINT,
    ADD COLUMN proof_l1_block_hash TEXT,
    ADD CONSTRAINT via_votable_proof_pair CHECK (
        (proof_l1_block_number IS NULL AND proof_l1_block_hash IS NULL)
        OR (proof_l1_block_number BETWEEN 0 AND 4294967295
            AND proof_l1_block_number IS NOT NULL AND proof_l1_block_hash IS NOT NULL)
    ),
    ADD CONSTRAINT via_votable_source_pair CHECK (
        (source_l1_block_number IS NULL AND source_l1_block_hash IS NULL)
        OR (source_l1_block_number BETWEEN 0 AND 4294967295
            AND source_l1_block_number IS NOT NULL AND source_l1_block_hash IS NOT NULL)
    );

-- Immutable proof anchors and individual votes allow a vote-only reorg to
-- retain canonical proofs without inventing provenance for legacy rows.
ALTER TABLE via_votes
    ADD COLUMN source_l1_block_number BIGINT,
    ADD COLUMN source_l1_block_hash TEXT,
    ADD CONSTRAINT via_vote_source_pair CHECK (
        (source_l1_block_number IS NULL AND source_l1_block_hash IS NULL)
        OR (source_l1_block_number BETWEEN 0 AND 4294967295
            AND source_l1_block_number IS NOT NULL AND source_l1_block_hash IS NOT NULL)
    );
CREATE INDEX via_votable_proof_height ON via_votable_transactions(proof_l1_block_number)
    WHERE proof_l1_block_number IS NOT NULL;
CREATE INDEX via_votes_source_height ON via_votes(source_l1_block_number)
    WHERE source_l1_block_number IS NOT NULL;

-- Legacy facts remain forensic history, never authority. Statement triggers also fence
-- old binaries on empty writes; TRUNCATE must not erase quarantined evidence.
CREATE FUNCTION via_reject_legacy_withdrawal_write() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'legacy withdrawal storage is quarantined; coordinated lifecycle cutover required';
END;
$$;
CREATE TRIGGER via_withdrawals_quarantined BEFORE INSERT OR UPDATE OR DELETE OR TRUNCATE ON via_withdrawals FOR EACH STATEMENT EXECUTE FUNCTION via_reject_legacy_withdrawal_write();
CREATE TRIGGER via_bridge_withdrawals_quarantined BEFORE INSERT OR UPDATE OR DELETE OR TRUNCATE ON via_bridge_withdrawals FOR EACH STATEMENT EXECUTE FUNCTION via_reject_legacy_withdrawal_write();
CREATE TRIGGER via_batch_withdrawals_quarantined BEFORE INSERT OR UPDATE OR DELETE OR TRUNCATE ON via_l1_batch_bridge_withdrawals FOR EACH STATEMENT EXECUTE FUNCTION via_reject_legacy_withdrawal_write();

-- This copies only a negative quarantine, never legacy amounts, origins or paid
-- flags into authority. Reconstructed complete imports cannot erase old risk.
CREATE TABLE via_withdrawal_legacy_holds (reference TEXT PRIMARY KEY);
INSERT INTO via_withdrawal_legacy_holds(reference) SELECT id FROM via_withdrawals;

-- One verifier database serves one chain. Wallet rotation requires an explicit,
-- separately reviewed reconciliation; a new script must not reset obligations.
CREATE TABLE via_withdrawal_active_wallet (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    wallet BYTEA NOT NULL CHECK (octet_length(wallet) > 0)
);
CREATE TABLE via_withdrawal_authority_domain (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    domain JSONB NOT NULL
);

CREATE TABLE via_withdrawal_batches (
    wallet BYTEA NOT NULL,
    batch_number BIGINT NOT NULL CHECK (batch_number BETWEEN 0 AND 4294967295),
    proof_txid BYTEA NOT NULL CHECK (octet_length(proof_txid) = 32),
    blob_id TEXT NOT NULL,
    evidence JSONB NOT NULL,
    invalidated BOOLEAN NOT NULL DEFAULT FALSE,
    conflicted BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (wallet, batch_number),
    UNIQUE (wallet, proof_txid)
);
-- Full origins include non-payable evidence, so a later batch cannot relabel it.
CREATE TABLE via_withdrawal_origins (
    wallet BYTEA NOT NULL,
    origin_txid BYTEA NOT NULL CHECK (octet_length(origin_txid) = 32),
    origin_index INTEGER NOT NULL CHECK (origin_index BETWEEN 0 AND 65535),
    reference TEXT NOT NULL CHECK (reference ~ '^[0-9a-f]{20}$'),
    batch_number BIGINT NOT NULL,
    PRIMARY KEY (wallet, origin_txid, origin_index),
    FOREIGN KEY (wallet, batch_number) REFERENCES via_withdrawal_batches(wallet, batch_number)
);
CREATE INDEX via_withdrawal_origins_reference ON via_withdrawal_origins(wallet, reference);
CREATE TABLE via_withdrawal_expected (
    wallet BYTEA NOT NULL,
    origin_txid BYTEA NOT NULL CHECK (octet_length(origin_txid) = 32),
    origin_index INTEGER NOT NULL CHECK (origin_index BETWEEN 0 AND 65535),
    reference TEXT NOT NULL CHECK (reference ~ '^[0-9a-f]{20}$'),
    batch_number BIGINT NOT NULL,
    snapshot JSONB NOT NULL,
    gross NUMERIC(20,0) NOT NULL CHECK (gross >= 0 AND gross <= 18446744073709551615),
    PRIMARY KEY (wallet, origin_txid, origin_index),
    FOREIGN KEY (wallet, batch_number) REFERENCES via_withdrawal_batches(wallet, batch_number),
    FOREIGN KEY (wallet, origin_txid, origin_index) REFERENCES via_withdrawal_origins(wallet, origin_txid, origin_index)
);
CREATE INDEX via_withdrawal_expected_reference ON via_withdrawal_expected(wallet, reference);
CREATE TABLE via_withdrawal_conflicts (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    wallet BYTEA NOT NULL,
    kind TEXT NOT NULL,
    identity BYTEA NOT NULL,
    evidence JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE via_withdrawal_holds (
    wallet BYTEA NOT NULL,
    reference TEXT NOT NULL,
    reason TEXT NOT NULL,
    source BYTEA NOT NULL,
    PRIMARY KEY (wallet, reference, reason, source)
);
CREATE TABLE via_withdrawal_observations (
    wallet BYTEA NOT NULL,
    txid BYTEA NOT NULL CHECK (octet_length(txid) = 32),
    evidence JSONB NOT NULL,
    conflicted BOOLEAN NOT NULL DEFAULT FALSE,
    uncertain BOOLEAN NOT NULL DEFAULT FALSE,
    accepted_block BYTEA,
    accepted_height BIGINT,
    fulfilled BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (wallet, txid),
    CHECK ((accepted_block IS NULL) = (accepted_height IS NULL))
);
CREATE TABLE via_withdrawal_outputs (
    wallet BYTEA NOT NULL,
    txid BYTEA NOT NULL,
    vout BIGINT NOT NULL CHECK (vout BETWEEN 0 AND 4294967295),
    reference TEXT NOT NULL CHECK (reference ~ '^[0-9a-f]{20}$'),
    script BYTEA NOT NULL,
    amount NUMERIC(20,0) NOT NULL,
    PRIMARY KEY (wallet, txid, vout),
    FOREIGN KEY (wallet, txid) REFERENCES via_withdrawal_observations(wallet, txid)
);
CREATE INDEX via_withdrawal_outputs_reference ON via_withdrawal_outputs(wallet, reference);
CREATE TABLE via_withdrawal_observed_inputs (
    wallet BYTEA NOT NULL,
    txid BYTEA NOT NULL,
    prev_txid BYTEA NOT NULL,
    prev_vout BIGINT NOT NULL,
    PRIMARY KEY (wallet, txid, prev_txid, prev_vout),
    FOREIGN KEY (wallet, txid) REFERENCES via_withdrawal_observations(wallet, txid)
);
CREATE INDEX via_withdrawal_observed_inputs_outpoint ON via_withdrawal_observed_inputs(wallet, prev_txid, prev_vout);
CREATE TABLE via_withdrawal_inclusions (
    wallet BYTEA NOT NULL,
    txid BYTEA NOT NULL,
    block_hash BYTEA NOT NULL CHECK (octet_length(block_hash) = 32),
    block_height BIGINT NOT NULL CHECK (block_height BETWEEN 0 AND 4294967295),
    block_hash_text TEXT NOT NULL,
    PRIMARY KEY (wallet, txid, block_hash, block_height),
    FOREIGN KEY (wallet, txid) REFERENCES via_withdrawal_observations(wallet, txid)
);
CREATE TABLE via_withdrawal_fulfillments (
    wallet BYTEA NOT NULL,
    reference TEXT NOT NULL,
    origin_txid BYTEA NOT NULL,
    origin_index INTEGER NOT NULL,
    block_hash BYTEA NOT NULL,
    block_height BIGINT NOT NULL,
    txid BYTEA NOT NULL,
    vout BIGINT NOT NULL,
    accepted_tip BIGINT NOT NULL,
    confirmations BIGINT NOT NULL CHECK (confirmations > 0),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    PRIMARY KEY (wallet, origin_txid, origin_index, block_hash, block_height),
    UNIQUE (wallet, txid, vout, block_hash, block_height),
    FOREIGN KEY (wallet, origin_txid, origin_index) REFERENCES via_withdrawal_expected(wallet, origin_txid, origin_index),
    FOREIGN KEY (wallet, txid, vout) REFERENCES via_withdrawal_outputs(wallet, txid, vout),
    FOREIGN KEY (wallet, txid, block_hash, block_height) REFERENCES via_withdrawal_inclusions(wallet, txid, block_hash, block_height)
);
CREATE UNIQUE INDEX via_withdrawal_one_active_fulfillment ON via_withdrawal_fulfillments(wallet, origin_txid, origin_index) WHERE active;
CREATE TABLE via_withdrawal_attempts (
    wallet BYTEA NOT NULL,
    round_id BYTEA NOT NULL CHECK (octet_length(round_id) = 32),
    content BYTEA NOT NULL CHECK (octet_length(content) > 0),
    requests JSONB NOT NULL,
    inputs JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('admitted','may_have_signed','signed','finalized','retired')),
    signed_risk BOOLEAN NOT NULL DEFAULT FALSE,
    coordinator_exposed BOOLEAN NOT NULL DEFAULT FALSE,
    public_nonces BYTEA,
    public_signatures BYTEA,
    finalized_transaction BYTEA,
    finalized_txid BYTEA CHECK (octet_length(finalized_txid) = 32),
    PRIMARY KEY (wallet, round_id),
    CHECK (state NOT IN ('may_have_signed','signed','finalized') OR signed_risk),
    CHECK (state NOT IN ('signed','finalized') OR public_signatures IS NOT NULL),
    CHECK (state <> 'finalized' OR (finalized_transaction IS NOT NULL AND finalized_txid IS NOT NULL))
);
CREATE TABLE via_withdrawal_request_reservations (
    wallet BYTEA NOT NULL,
    reference TEXT NOT NULL,
    origin_txid BYTEA NOT NULL,
    origin_index INTEGER NOT NULL,
    round_id BYTEA NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    PRIMARY KEY (wallet, reference, round_id),
    FOREIGN KEY (wallet, origin_txid, origin_index) REFERENCES via_withdrawal_expected(wallet, origin_txid, origin_index),
    FOREIGN KEY (wallet, round_id) REFERENCES via_withdrawal_attempts(wallet, round_id)
);
CREATE UNIQUE INDEX via_withdrawal_one_request_reservation ON via_withdrawal_request_reservations(wallet, reference) WHERE active;
CREATE TABLE via_withdrawal_input_reservations (
    wallet BYTEA NOT NULL,
    prev_txid BYTEA NOT NULL,
    prev_vout BIGINT NOT NULL,
    round_id BYTEA NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    PRIMARY KEY (wallet, prev_txid, prev_vout, round_id),
    FOREIGN KEY (wallet, round_id) REFERENCES via_withdrawal_attempts(wallet, round_id)
);
CREATE UNIQUE INDEX via_withdrawal_one_input_reservation ON via_withdrawal_input_reservations(wallet, prev_txid, prev_vout) WHERE active;
