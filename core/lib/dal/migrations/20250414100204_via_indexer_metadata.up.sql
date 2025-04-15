CREATE TABLE IF NOT EXISTS via_indexer_metadata (
    module VARCHAR UNIQUE NOT NULL,
    last_indexer_l1_block BIGINT NOT NULL,
    updated_at TIMESTAMP NOT NULL
);
