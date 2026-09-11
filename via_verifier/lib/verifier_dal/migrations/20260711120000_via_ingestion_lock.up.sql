-- Serialization point that exists before the first block is applied.
-- The checkpoint row only exists after genesis, so locking it cannot
-- serialize two concurrent genesis applies; this pre-seeded row can.
-- Halt recording takes the same lock, closing the halt-vs-apply race.
CREATE TABLE via_ingestion_lock (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id)
);
INSERT INTO via_ingestion_lock (id) VALUES (TRUE);

ALTER TABLE via_ingestion_checkpoint
    ADD CONSTRAINT via_ingestion_checkpoint_height_nonneg CHECK (height >= 0),
    ADD CONSTRAINT via_ingestion_checkpoint_revision_nonneg CHECK (canonical_revision >= 0);
ALTER TABLE via_ingestion_chain
    ADD CONSTRAINT via_ingestion_chain_height_nonneg CHECK (height >= 0);
ALTER TABLE via_ingestion_tracked_outputs
    ADD CONSTRAINT via_ingestion_tracked_outputs_value_nonneg CHECK (value_sat >= 0);
ALTER TABLE via_ingestion_deposits
    ADD CONSTRAINT via_ingestion_deposits_amount_nonneg CHECK (amount_sat >= 0);
ALTER TABLE via_ingestion_withdrawals
    ADD CONSTRAINT via_ingestion_withdrawals_amount_nonneg CHECK (amount_sat >= 0);
