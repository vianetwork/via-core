DROP TABLE IF EXISTS via_l1_reorg;
DROP TABLE IF EXISTS via_l1_blocks;

BEGIN;
    DROP INDEX IF EXISTS idx_via_wallets_l1_block_number;
    ALTER TABLE via_wallets
    DROP COLUMN IF EXISTS l1_block_number;
COMMIT;

BEGIN;
    DROP INDEX IF EXISTS idx_via_transactions_l1_block_number;
    ALTER TABLE via_transactions
    DROP COLUMN IF EXISTS l1_batch_number;
    ALTER TABLE via_transactions
    DROP COLUMN IF EXISTS l1_block_number;
COMMIT;
