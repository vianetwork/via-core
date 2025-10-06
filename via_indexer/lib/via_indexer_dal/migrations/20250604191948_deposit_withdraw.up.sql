CREATE TABLE IF NOT EXISTS deposits (
    "priority_id" BIGINT NOT NULL,
    "tx_id" BYTEA NOT NULL,
    "block_number" BIGINT NOT NULL,
    "sender" VARCHAR NOT NULL,
    "receiver" VARCHAR NOT NULL,
    "value" BIGINT NOT NULL,
    "calldata" BYTEA,
    "canonical_tx_hash" BYTEA NOT NULL UNIQUE,
    "created_at" BIGINT NOT NULL,
    PRIMARY KEY (tx_id)
);

CREATE TABLE IF NOT EXISTS withdrawals (
    "id" VARCHAR UNIQUE NOT NULL,
    "tx_id" BYTEA NOT NULL,
    "l2_tx_log_index" BIGINT NOT NULL,
    "receiver" VARCHAR NOT NULL,
    "value" BIGINT NOT NULL,
    "block_number" BIGINT NOT NULL,
    "timestamp" BIGINT NOT NULL,
    "created_at" TIMESTAMP NOT NULL DEFAULT NOW()
);
