CREATE TABLE IF NOT EXISTS via_votable_transactions (
    id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    l1_batch_number BIGINT NOT NULL,
    l1_batch_hash BYTEA UNIQUE NOT NULL,
    prev_l1_batch_hash BYTEA NOT NULL,
    proof_blob_id VARCHAR UNIQUE NOT NULL,
    proof_reveal_tx_id BYTEA UNIQUE NOT NULL,
    pubdata_blob_id VARCHAR UNIQUE NOT NULL,
    pubdata_reveal_tx_id VARCHAR UNIQUE NOT NULL,
    da_identifier VARCHAR NOT NULL,
    bridge_tx_id BYTEA,
    is_finalized BOOLEAN,
    l1_batch_status BOOLEAN,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS via_votes (
    id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    votable_transaction_id BIGINT NOT NULL,
    verifier_address TEXT NOT NULL,
    vote BOOLEAN NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    UNIQUE (votable_transaction_id, verifier_address) 
);

ALTER TABLE "via_l1_batch_vote_inscription_request" ADD FOREIGN KEY ("votable_transaction_id") REFERENCES "via_votable_transactions" ("id") ON DELETE CASCADE ON UPDATE NO ACTION;
ALTER TABLE "via_votes" ADD FOREIGN KEY ("votable_transaction_id") REFERENCES "via_votable_transactions" ("id") ON DELETE CASCADE ON UPDATE NO ACTION;
CREATE INDEX idx_via_votable_transactions_l1_batch_hash ON via_votable_transactions(l1_batch_hash);
CREATE INDEX idx_via_votable_transactions_finalized ON via_votable_transactions (is_finalized) WHERE is_finalized IS NOT NULL;
CREATE INDEX idx_via_votable_transactions_status ON via_votable_transactions (l1_batch_status) WHERE l1_batch_status IS NOT NULL;
CREATE INDEX idx_via_votable_transactions_batch_tx ON via_votable_transactions (bridge_tx_id) WHERE bridge_tx_id IS NOT NULL;
