-- Allow duplicate batch numbers, but only one can be in canonical chain
CREATE UNIQUE INDEX unique_canonical_batch 
ON via_votable_transactions (l1_batch_number) 
WHERE is_finalized = TRUE || is_finalized IS NULL;