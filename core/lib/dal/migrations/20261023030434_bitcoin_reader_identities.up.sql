-- Chain identity and progress are created inert; stopped migration installs proved positions later.
CREATE TABLE via_btc_headers (
    hash BYTEA PRIMARY KEY CHECK (octet_length(hash) = 32),
    height BIGINT NOT NULL CHECK (height BETWEEN 0 AND 4294967295),
    parent_hash BYTEA NOT NULL CHECK (octet_length(parent_hash) = 32),
    UNIQUE (height, hash)
);
CREATE INDEX via_btc_headers_height_idx ON via_btc_headers (height);

CREATE TABLE via_btc_migration_certificates (
    id BIGSERIAL PRIMARY KEY,
    format_version INTEGER NOT NULL CHECK (format_version > 0),
    network TEXT NOT NULL,
    genesis BYTEA NOT NULL CHECK (octet_length(genesis) = 32),
    scope TEXT NOT NULL CHECK (scope IN ('configuration', 'deposits', 'proofs_and_votes', 'withdrawals', 'cutover')),
    body JSONB NOT NULL CHECK (jsonb_typeof(body) = 'object'),
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    UNIQUE (id, scope)
);

CREATE TABLE via_btc_scan_positions (
    group_name TEXT PRIMARY KEY CHECK (group_name IN ('configuration', 'deposits', 'proofs_and_votes', 'withdrawals')),
    height BIGINT NOT NULL,
    hash BYTEA NOT NULL,
    certificate_id BIGINT NOT NULL,
    updated_at TIMESTAMP NOT NULL DEFAULT NOW(),
    FOREIGN KEY (certificate_id, group_name) REFERENCES via_btc_migration_certificates(id, scope),
    FOREIGN KEY (height, hash) REFERENCES via_btc_headers(height, hash)
);

-- A failed CAS aborts earlier effects in the same transaction, even if a caller catches its error.
-- https://github.com/Postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/doc/src/sgml/plpgsql.sgml
CREATE FUNCTION via_btc_compare_and_swap_position(
    scan_group TEXT, expected_height BIGINT, expected_hash BYTEA, next_height BIGINT, next_hash BYTEA
) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
    UPDATE via_btc_scan_positions SET height = next_height, hash = next_hash, updated_at = NOW()
    WHERE group_name = scan_group AND height = expected_height AND hash = expected_hash;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'Bitcoin reader position changed for group %', scan_group USING ERRCODE = '40001';
    END IF;
END;
$$;
