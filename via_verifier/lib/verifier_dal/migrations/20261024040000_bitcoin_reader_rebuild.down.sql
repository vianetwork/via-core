ALTER TABLE via_votes DROP CONSTRAINT via_votes_votable_transaction_id_fkey,
    ADD CONSTRAINT via_votes_votable_transaction_id_fkey FOREIGN KEY (votable_transaction_id) REFERENCES via_votable_transactions(id) ON DELETE CASCADE;
ALTER TABLE via_l1_batch_vote_inscription_request DROP CONSTRAINT via_l1_batch_vote_inscription_reque_votable_transaction_id_fkey,
    ADD CONSTRAINT via_l1_batch_vote_inscription_reque_votable_transaction_id_fkey FOREIGN KEY (votable_transaction_id) REFERENCES via_votable_transactions(id) ON DELETE CASCADE;
ALTER TABLE via_btc_inscriptions_request_history DROP CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey,
    ADD CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey FOREIGN KEY (inscription_request_id) REFERENCES via_btc_inscriptions_request(id) ON DELETE CASCADE;
DROP INDEX via_transactions_outpoint;
ALTER TABLE via_transactions ADD PRIMARY KEY (tx_id);
DROP INDEX via_transactions_l1_block_number_bitcoin_hash_idx;
ALTER TABLE via_transactions DROP COLUMN bitcoin_height, DROP COLUMN bitcoin_hash, DROP COLUMN bitcoin_vout, DROP COLUMN bitcoin_payload;
ALTER TABLE protocol_versions DROP COLUMN bitcoin_height, DROP COLUMN bitcoin_hash, DROP COLUMN bitcoin_payload;
ALTER TABLE protocol_patches DROP COLUMN bitcoin_height, DROP COLUMN bitcoin_hash, DROP COLUMN bitcoin_payload;
ALTER TABLE via_votable_transactions DROP COLUMN proof_header_id, DROP COLUMN bitcoin_payload,
    DROP COLUMN transaction_index, DROP COLUMN message_index;
DROP INDEX via_votable_proof_inclusion;
CREATE UNIQUE INDEX unique_canonical_batch ON via_votable_transactions(l1_batch_number)
    WHERE is_finalized=TRUE OR is_finalized IS NULL;
ALTER TABLE via_votable_transactions ADD UNIQUE(l1_batch_hash), ADD UNIQUE(proof_blob_id),
    ADD UNIQUE(proof_reveal_tx_id), ADD UNIQUE(pubdata_blob_id), ADD UNIQUE(pubdata_reveal_tx_id);
ALTER TABLE via_votes DROP COLUMN txid, DROP COLUMN transaction_index, DROP COLUMN message_index, DROP COLUMN bitcoin_payload;
ALTER TABLE via_wallets DROP COLUMN block_hash, DROP COLUMN transaction_index, DROP COLUMN message_index;
DROP TABLE via_btc_configuration, via_btc_consumers, via_btc_reader, via_btc_headers;
