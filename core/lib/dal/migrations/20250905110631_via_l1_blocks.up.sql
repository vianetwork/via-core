BEGIN;
    ALTER TABLE via_wallets
    ADD COLUMN l1_block_number BIGINT NOT NULL DEFAULT 0;
    
    CREATE INDEX idx_via_wallets_l1_block_number
    ON via_wallets(l1_block_number);
COMMIT;

CREATE TABLE via_l1_blocks (
    "number" BIGINT UNIQUE NOT NULL,
    "hash" VARCHAR UNIQUE NOT NULL
);

CREATE TABLE via_l1_reorg (
    "l1_block_number" BIGINT UNIQUE NOT NULL,
    "l1_batch_number" BIGINT UNIQUE NOT NULL,
    "created_at" TIMESTAMPTZ NOT NULL DEFAULT now()
);
