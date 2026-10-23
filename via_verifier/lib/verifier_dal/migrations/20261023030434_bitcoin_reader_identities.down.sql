-- Retained ancestry and certificates cannot be represented by the legacy scalar schema.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM via_btc_headers) OR EXISTS (SELECT 1 FROM via_btc_migration_certificates) THEN
        RAISE EXCEPTION 'Bitcoin reader state requires a stopped, proved rollback';
    END IF;
END $$;
DROP FUNCTION via_btc_compare_and_swap_position(TEXT, BIGINT, BYTEA, BIGINT, BYTEA);
DROP TABLE via_btc_scan_positions;
DROP TABLE via_btc_migration_certificates;
DROP TABLE via_btc_headers;
