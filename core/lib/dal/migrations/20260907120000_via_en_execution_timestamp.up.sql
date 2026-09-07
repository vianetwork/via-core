ALTER TABLE l1_batches ADD COLUMN via_en_executed_at TIMESTAMP;

COMMENT ON COLUMN l1_batches.via_en_executed_at IS
    'Per-batch execution time synchronized from an explicitly accepted Via verdict. NULL legacy rows require source revalidation, not a copy from the shared execution sentinel history.';
