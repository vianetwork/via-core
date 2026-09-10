CREATE TABLE IF NOT EXISTS via_btc_tx_locators (
    tx_id VARCHAR PRIMARY KEY,
    l1_block_number BIGINT NOT NULL,
    l1_block_hash VARCHAR NOT NULL,
    tx_index INTEGER,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_via_btc_tx_locators_l1_block_number
    ON via_btc_tx_locators(l1_block_number);
