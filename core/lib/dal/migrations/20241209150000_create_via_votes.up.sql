CREATE TABLE IF NOT EXISTS via_votes (
    l1_batch_number BIGINT NOT NULL,
    proof_reveal_tx_id BYTEA NOT NULL,    
    verifier_address TEXT NOT NULL,
    vote BOOLEAN NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    PRIMARY KEY(l1_batch_number, proof_reveal_tx_id, verifier_address)
);

ALTER TABLE "via_votes" ADD FOREIGN KEY ("l1_batch_number") REFERENCES "l1_batches" ("number") ON DELETE CASCADE ON UPDATE NO ACTION;
