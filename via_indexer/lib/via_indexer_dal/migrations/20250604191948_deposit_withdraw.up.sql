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

CREATE TABLE IF NOT EXISTS bridge_withdrawals (
    "id" SERIAL PRIMARY KEY,
    "tx_id" BYTEA NOT NULL UNIQUE,
    "l1_batch_reveal_tx_id" BYTEA NOT NULL,
    "fee" BIGINT NOT NULL,
    "vsize" BIGINT NOT NULL,
    "total_size" BIGINT NOT NULL,
    "withdrawals_count" BIGINT NOT NULL,
    "block_number" BIGINT NOT NULL,
    "created_at" TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS withdrawals (
    "id" SERIAL PRIMARY KEY,
    "bridge_withdrawal_id" INTEGER NOT NULL,
    "tx_index" BIGINT NOT NULL,
    "receiver" VARCHAR NOT NULL,
    "value" BIGINT NOT NULL,
    "created_at" TIMESTAMP NOT NULL DEFAULT NOW(),
    FOREIGN KEY (bridge_withdrawal_id) REFERENCES bridge_withdrawals (id) ON DELETE CASCADE
);
