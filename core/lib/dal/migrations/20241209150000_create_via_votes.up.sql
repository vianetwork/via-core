CREATE TABLE IF NOT EXISTS via_votable_transactions (
    l1_batch_number BIGINT NOT NULL REFERENCES l1_batches (number) ON DELETE CASCADE,
    tx_id BYTEA,
    is_finalized BOOLEAN NOT NULL DEFAULT FALSE,
    is_verified BOOLEAN NOT NULL DEFAULT FALSE,
    l1_batch_status BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW(),
    PRIMARY KEY (l1_batch_number, tx_id)
);

CREATE TABLE IF NOT EXISTS via_votes (
    l1_batch_number BIGINT NOT NULL,
    tx_id BYTEA NOT NULL,
    verifier_address TEXT NOT NULL,
    vote BOOLEAN NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    PRIMARY KEY (l1_batch_number, tx_id, verifier_address),
    FOREIGN KEY (l1_batch_number, tx_id) REFERENCES via_votable_transactions (l1_batch_number, tx_id) ON DELETE CASCADE
    );

