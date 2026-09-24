-- Legacy rows retain their unknown output identity; never invent a vout for them.
ALTER TABLE withdrawals DROP CONSTRAINT withdrawals_id_key;
ALTER TABLE withdrawals ADD COLUMN vout BIGINT;
ALTER TABLE withdrawals ADD CONSTRAINT withdrawals_output_identity UNIQUE (tx_id, vout);
ALTER TABLE withdrawals ADD CONSTRAINT withdrawals_vout_required
    CHECK (vout IS NOT NULL AND vout >= 0 AND vout <= 4294967295) NOT VALID;
CREATE INDEX withdrawals_reference ON withdrawals (id);
