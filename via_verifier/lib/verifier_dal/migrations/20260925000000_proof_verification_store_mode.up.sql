-- One row permanently designates the store for real or development proof verification on one Bitcoin chain.
-- It lives in the database it governs, so a copied or restored store keeps its designation.
-- Geth likewise keeps its chain config inside the chain database, keyed by the genesis hash:
-- https://github.com/ethereum/go-ethereum/blob/920c07774c65ebb3023536f85df642c44478b540/core/rawdb/accessors_metadata.go#L57-L82
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
-- The marker stays on the row, so no later reader can mistake an unproven approval for a verified one.
-- RISC Zero likewise keeps a fake receipt a distinct kind that passes only in a development context:
-- https://github.com/risc0/risc0/blob/218e3bc4a8ffcd203a9cd4e46f921bf60aa7e2bd/risc0/zkvm/src/receipt.rs#L438-L448
ALTER TABLE via_votable_transactions
    ADD COLUMN IF NOT EXISTS unverified_dev BOOLEAN NOT NULL DEFAULT FALSE;
