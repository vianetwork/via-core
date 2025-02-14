CREATE TABLE IF NOT EXISTS via_transactions (
    "priority_id" BIGINT NOT NULL,
    "tx_id" BYTEA NOT NULL,
    "receiver" VARCHAR NOT NULL,
    "value" BIGINT NOT NULL,
    "calldata" BYTEA,
    "canonical_tx_hash" BYTEA NOT NULL,
    "status"  BOOLEAN,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tx_id)
);

CREATE INDEX idx_via_transactions_priority ON via_transactions(priority_id);
