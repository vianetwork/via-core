-- The singleton designation preserves proof mode and Bitcoin-chain identity across restores.
CREATE TABLE IF NOT EXISTS via_verifier_store_mode (
    id SMALLINT PRIMARY KEY CHECK (id = 1),
    proof_verification_dev_mode BOOLEAN NOT NULL,
    bitcoin_network TEXT NOT NULL,
    bitcoin_genesis_hash TEXT NOT NULL,
    designated_after_votable_id BIGINT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

-- TRUE marks proof-free development acceptance; FALSE does not establish cryptographic verification.
ALTER TABLE via_votable_transactions
    ADD COLUMN IF NOT EXISTS unverified_dev BOOLEAN NOT NULL DEFAULT FALSE;
