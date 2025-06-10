CREATE TABLE IF NOT EXISTS indexer_metadata (
    module VARCHAR NOT NULL UNIQUE,
    last_indexer_l1_block BIGINT NOT NULL,
    updated_at TIMESTAMP NOT NULL
);
