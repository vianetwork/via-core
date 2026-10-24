DROP TABLE via_btc_finalizations, via_btc_proofs;
DROP INDEX transactions_l1_block_number_bitcoin_hash_idx;
ALTER TABLE transactions DROP COLUMN bitcoin_height, DROP COLUMN bitcoin_hash, DROP COLUMN bitcoin_vout, DROP COLUMN bitcoin_payload;
ALTER TABLE via_votes DROP COLUMN l1_block_number, DROP COLUMN block_hash, DROP COLUMN txid, DROP COLUMN transaction_index, DROP COLUMN message_index, DROP COLUMN bitcoin_payload;
ALTER TABLE protocol_patches DROP COLUMN bitcoin_height, DROP COLUMN bitcoin_hash, DROP COLUMN bitcoin_payload;
ALTER TABLE via_wallets DROP COLUMN block_hash, DROP COLUMN transaction_index, DROP COLUMN message_index;
DROP TABLE via_btc_configuration, via_btc_consumers, via_btc_reader, via_btc_headers;
