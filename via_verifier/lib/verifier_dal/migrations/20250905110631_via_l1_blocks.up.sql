BEGIN;
    ALTER TABLE via_transactions
    ADD COLUMN l1_block_number BIGINT NOT NULL DEFAULT 0;
    
    ALTER TABLE via_transactions
    ADD COLUMN l1_batch_number BIGINT DEFAULT NULL;

    CREATE INDEX idx_via_transactions_l1_block_number
    ON via_transactions(l1_block_number);
COMMIT;

BEGIN;
    ALTER TABLE via_wallets
    ADD COLUMN l1_block_number BIGINT NOT NULL DEFAULT 0;
    
    CREATE INDEX idx_via_wallets_l1_block_number
    ON via_wallets(l1_block_number);
COMMIT;

ALTER TABLE via_bridge_tx
DROP CONSTRAINT via_bridge_tx_votable_tx_id_fkey,
ADD CONSTRAINT via_bridge_tx_votable_tx_id_fkey
    FOREIGN KEY ("votable_tx_id")
    REFERENCES via_votable_transactions(id)
    ON DELETE CASCADE;

CREATE TABLE via_l1_blocks (
    "number" BIGINT UNIQUE NOT NULL,
    "hash" VARCHAR UNIQUE NOT NULL
);

CREATE TABLE via_l1_reorg (
    "l1_block_number" BIGINT UNIQUE NOT NULL,
    "l1_batch_number" BIGINT UNIQUE NOT NULL,
    "created_at" TIMESTAMPTZ NOT NULL DEFAULT now()
);
