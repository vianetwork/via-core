-- One row permanently designates the store for real or development proof verification on one Bitcoin chain.
-- Verdicts with an id above designated_after_votable_id were written under this designation.
CREATE TABLE IF NOT EXISTS via_verifier_store_mode (
    id SMALLINT PRIMARY KEY CHECK (id = 1),
    proof_verification_dev_mode BOOLEAN NOT NULL,
    bitcoin_network TEXT NOT NULL,
    bitcoin_genesis_hash TEXT NOT NULL,
    designated_after_votable_id BIGINT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

-- TRUE records that development mode accepted the result without a proof.
-- FALSE only means development acceptance did not write it, not that the result was proven.
ALTER TABLE via_votable_transactions
    ADD COLUMN IF NOT EXISTS unverified_dev BOOLEAN NOT NULL DEFAULT FALSE;
