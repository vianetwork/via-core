ALTER TABLE via_l1_batch_inscription_request DROP COLUMN rotation_legacy_cutoff;
ALTER TABLE via_btc_proofs DROP COLUMN proof_header_id;
DROP INDEX via_vote_occurrence;
DROP INDEX via_vote_member;
DROP INDEX via_vote_legacy;
ALTER TABLE via_votes DROP COLUMN votable_transaction_id, DROP COLUMN id;
ALTER TABLE via_votes RENAME COLUMN source_l1_block_number TO l1_block_number;
ALTER TABLE via_votes RENAME COLUMN source_l1_block_hash TO block_hash;
ALTER TABLE via_votes ALTER COLUMN l1_batch_number SET NOT NULL, ALTER COLUMN proof_reveal_tx_id SET NOT NULL;
ALTER TABLE via_votes ADD PRIMARY KEY(l1_batch_number,proof_reveal_tx_id,verifier_address),
    ADD FOREIGN KEY(l1_batch_number) REFERENCES l1_batches(number) ON DELETE CASCADE;
DROP INDEX via_wallet_occurrence;
DROP INDEX via_wallet_legacy;
ALTER TABLE via_wallets ADD CONSTRAINT unique_tx_hash_address_role UNIQUE(tx_hash,address,role);
