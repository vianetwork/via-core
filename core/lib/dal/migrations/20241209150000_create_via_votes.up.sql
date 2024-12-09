CREATE TABLE IF NOT EXISTS via_votable_transactions (
    tx_id BYTEA PRIMARY KEY,
    transaction_type TEXT NOT NULL,
    is_finalized BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS via_votes (
    tx_id BYTEA NOT NULL REFERENCES via_votable_transactions(tx_id) ON DELETE CASCADE,
    verifier_address TEXT NOT NULL,
    vote BOOLEAN NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),

    PRIMARY KEY (tx_id, verifier_address)
);
