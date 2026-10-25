DROP INDEX via_transactions_batch;
ALTER TABLE via_votable_transactions DROP COLUMN rotation_legacy_cutoff;
DROP INDEX via_vote_occurrence;
DROP INDEX via_wallet_occurrence;
DROP INDEX via_wallet_legacy;
ALTER TABLE via_wallets ADD CONSTRAINT unique_tx_hash_address_role UNIQUE(tx_hash,address,role);
