-- A stopped, unused reader schema may be reverted. Evidence is never deleted to make it fit an old format.
-- Nonempty stores require the separately approved backup / forward-repair route.
DO $$
DECLARE relation_name TEXT; populated BOOLEAN;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY['via_wallet_inclusions','via_deposit_inclusions','via_deposit_evidence','via_proof_finalization','via_proof_inclusions','via_proof_evidence','via_vote_inclusions','via_vote_evidence','via_upgrade_inclusions','via_upgrade_evidence','via_btc_scan_positions','via_btc_migration_certificates','via_wallets','via_l1_blocks','via_legacy_votes','protocol_patches','via_btc_inscriptions_request','via_btc_inscriptions_request_history','via_proof_verdicts','via_transactions','via_withdrawal_inclusions','via_l1_batch_vote_inscription_request']
    LOOP
        EXECUTE format('SELECT EXISTS(SELECT 1 FROM %I)',relation_name) INTO populated;
        IF populated THEN RAISE EXCEPTION 'reader evidence is not representable by the previous schema: %',relation_name; END IF;
    END LOOP;

END;
$$;
DROP FUNCTION via_btc_request_is_eligible(BIGINT);
DROP VIEW via_votable_transactions;
DROP VIEW via_current_proof_inclusions;
DROP TABLE via_proof_l2_exclusions;
ALTER TABLE via_proof_verdicts DROP COLUMN reader_proof_inclusion_id;
ALTER TABLE via_proof_verdicts DROP COLUMN completed_input_identity;
ALTER TABLE via_proof_verdicts RENAME TO via_votable_transactions;
ALTER TABLE via_votable_transactions ADD UNIQUE(l1_batch_hash), ADD UNIQUE(proof_blob_id), ADD UNIQUE(proof_reveal_tx_id), ADD UNIQUE(pubdata_blob_id), ADD UNIQUE(pubdata_reveal_tx_id);
CREATE UNIQUE INDEX unique_canonical_batch ON via_votable_transactions(l1_batch_number) WHERE is_finalized=TRUE OR is_finalized IS NULL;
ALTER TABLE via_legacy_votes RENAME TO via_votes;
ALTER TABLE via_btc_inscriptions_request_history DROP CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey;
ALTER TABLE via_btc_inscriptions_request_history ADD CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey FOREIGN KEY(inscription_request_id) REFERENCES via_btc_inscriptions_request(id) ON DELETE CASCADE;
ALTER TABLE via_l1_batch_vote_inscription_request DROP CONSTRAINT via_l1_batch_vote_inscription_reque_votable_transaction_id_fkey;
ALTER TABLE via_l1_batch_vote_inscription_request ADD CONSTRAINT via_l1_batch_vote_inscription_reque_votable_transaction_id_fkey FOREIGN KEY(votable_transaction_id) REFERENCES via_votable_transactions(id) ON DELETE CASCADE;
ALTER TABLE via_votes DROP CONSTRAINT via_votes_votable_transaction_id_fkey;
ALTER TABLE via_votes ADD CONSTRAINT via_votes_votable_transaction_id_fkey FOREIGN KEY(votable_transaction_id) REFERENCES via_votable_transactions(id) ON DELETE CASCADE;
ALTER TABLE via_transactions DROP CONSTRAINT via_transactions_pkey;
ALTER TABLE via_transactions ADD PRIMARY KEY(tx_id);
ALTER TABLE via_votable_transactions ALTER COLUMN source_l1_block_hash TYPE TEXT USING NULL::TEXT;
ALTER TABLE via_votable_transactions ALTER COLUMN proof_l1_block_hash TYPE TEXT USING NULL::TEXT;
ALTER TABLE via_votes ALTER COLUMN source_l1_block_hash TYPE TEXT USING NULL::TEXT;
ALTER TABLE via_withdrawal_inclusions ADD COLUMN block_hash_text TEXT NOT NULL;
ALTER TABLE protocol_patches DROP COLUMN upgrade_tx_hash, DROP COLUMN executed;
DROP TABLE via_proof_finalization;
DROP TABLE via_vote_inclusions;
DROP TABLE via_vote_evidence;
DROP TABLE via_proof_inclusions;
DROP TABLE via_proof_evidence;
DROP TABLE via_wallet_inclusions;
DROP TABLE via_deposit_inclusions;
DROP TABLE via_deposit_evidence;
DROP TABLE via_upgrade_inclusions;
DROP TABLE via_upgrade_evidence;
ALTER TABLE via_wallets DROP CONSTRAINT via_wallet_identity;
ALTER TABLE via_wallets DROP COLUMN origin_kind, DROP COLUMN message_index, DROP COLUMN immutable_payload;
ALTER TABLE protocol_patches DROP COLUMN immutable_payload, DROP COLUMN via_bootstrap;
DROP FUNCTION via_btc_immutable_payload(JSONB,JSONB,TEXT);
ALTER TABLE via_l1_blocks DROP CONSTRAINT via_l1_blocks_number_hash_fkey;
ALTER TABLE via_l1_blocks DROP CONSTRAINT via_l1_blocks_hash_check;
ALTER TABLE via_l1_blocks ALTER COLUMN hash TYPE TEXT USING NULL::TEXT;
ALTER TABLE via_btc_scan_positions DROP CONSTRAINT via_btc_scan_positions_certificate_id_group_name_fkey;
ALTER TABLE via_btc_migration_certificates ALTER COLUMN id DROP DEFAULT;
ALTER TABLE via_btc_migration_certificates ALTER COLUMN id TYPE BIGINT USING NULL::BIGINT;
CREATE SEQUENCE via_btc_migration_certificates_id_seq OWNED BY via_btc_migration_certificates.id;
ALTER TABLE via_btc_migration_certificates ALTER COLUMN id SET DEFAULT nextval('via_btc_migration_certificates_id_seq');
ALTER TABLE via_btc_scan_positions ALTER COLUMN certificate_id TYPE BIGINT USING NULL::BIGINT;
ALTER TABLE via_btc_scan_positions ADD FOREIGN KEY(certificate_id,group_name) REFERENCES via_btc_migration_certificates(id,scope);
ALTER TABLE via_btc_migration_certificates DROP COLUMN application_status, DROP COLUMN applied_at;
DO $$
DECLARE constraint_name TEXT;
BEGIN
    FOR constraint_name IN SELECT conname FROM pg_constraint WHERE conrelid='via_btc_migration_certificates'::regclass AND pg_get_constraintdef(oid) LIKE '%octet_length%'
    LOOP EXECUTE format('ALTER TABLE via_btc_migration_certificates DROP CONSTRAINT %I',constraint_name); END LOOP;
END;
$$;
