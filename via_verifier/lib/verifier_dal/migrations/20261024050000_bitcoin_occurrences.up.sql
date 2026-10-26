ALTER TABLE via_wallets DROP CONSTRAINT unique_tx_hash_address_role;
CREATE UNIQUE INDEX via_wallet_legacy ON via_wallets(tx_hash,address,role) WHERE block_hash IS NULL;
CREATE UNIQUE INDEX via_wallet_occurrence ON via_wallets(block_hash,transaction_index,message_index,role);
CREATE UNIQUE INDEX via_vote_occurrence ON via_votes(source_l1_block_hash,transaction_index,message_index);
ALTER TABLE via_votable_transactions ADD COLUMN rotation_legacy_cutoff BIGINT;
CREATE INDEX via_transactions_batch ON via_transactions(l1_batch_number) WHERE l1_batch_number IS NOT NULL;
