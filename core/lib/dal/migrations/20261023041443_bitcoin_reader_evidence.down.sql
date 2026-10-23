-- A stopped, unused reader schema may be reverted. Evidence is never deleted to make it fit an old format.
-- Nonempty stores require the separately approved backup / forward-repair route.
DO $$
DECLARE relation_name TEXT; populated BOOLEAN;
BEGIN
    FOREACH relation_name IN ARRAY ARRAY['via_wallet_inclusions','via_deposit_inclusions','via_deposit_evidence','via_proof_finalization','via_proof_inclusions','via_proof_evidence','via_vote_inclusions','via_vote_evidence','via_upgrade_inclusions','via_upgrade_evidence','via_btc_scan_positions','via_btc_migration_certificates','via_wallets','via_l1_blocks','via_legacy_votes','protocol_patches','via_btc_inscriptions_request','via_btc_inscriptions_request_history','via_batch_delivery_facts']
    LOOP
        EXECUTE format('SELECT EXISTS(SELECT 1 FROM %I)',relation_name) INTO populated;
        IF populated THEN RAISE EXCEPTION 'reader evidence is not representable by the previous schema: %',relation_name; END IF;
    END LOOP;
    IF EXISTS(SELECT 1 FROM transactions WHERE is_priority) THEN RAISE EXCEPTION 'reader priority effects prevent downgrade'; END IF;
END;
$$;
DROP FUNCTION via_btc_request_is_eligible(BIGINT);
DROP VIEW via_l1_batch_inscription_request;
ALTER TABLE via_batch_delivery_facts DROP COLUMN source_batch_hash;
ALTER TABLE via_batch_delivery_facts RENAME TO via_l1_batch_inscription_request;
ALTER TABLE via_l1_batch_inscription_request ADD UNIQUE(l1_batch_number);
ALTER TABLE via_legacy_votes RENAME TO via_votes;
ALTER TABLE via_btc_inscriptions_request DROP CONSTRAINT via_btc_inscriptions_request_confirmed_inscriptions_reques_fkey;
ALTER TABLE via_btc_inscriptions_request ADD CONSTRAINT via_btc_inscriptions_request_confirmed_inscriptions_reques_fkey FOREIGN KEY(confirmed_inscriptions_request_history_id) REFERENCES via_btc_inscriptions_request_history(id) ON DELETE CASCADE;
ALTER TABLE via_btc_inscriptions_request_history DROP CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey;
ALTER TABLE via_btc_inscriptions_request_history ADD CONSTRAINT via_btc_inscriptions_request_histor_inscription_request_id_fkey FOREIGN KEY(inscription_request_id) REFERENCES via_btc_inscriptions_request(id) ON DELETE CASCADE;
ALTER TABLE via_l1_batch_inscription_request DROP CONSTRAINT via_l1_batch_inscription_requ_commit_l1_batch_inscription__fkey;
ALTER TABLE via_l1_batch_inscription_request ADD CONSTRAINT via_l1_batch_inscription_requ_commit_l1_batch_inscription__fkey FOREIGN KEY(commit_l1_batch_inscription_id) REFERENCES via_btc_inscriptions_request(id) ON DELETE CASCADE;
ALTER TABLE via_l1_batch_inscription_request DROP CONSTRAINT via_l1_batch_inscription_reque_commit_proof_inscription_id_fkey;
ALTER TABLE via_l1_batch_inscription_request ADD CONSTRAINT via_l1_batch_inscription_reque_commit_proof_inscription_id_fkey FOREIGN KEY(commit_proof_inscription_id) REFERENCES via_btc_inscriptions_request(id) ON DELETE CASCADE;
ALTER TABLE via_btc_inscriptions_request ADD CONSTRAINT via_btc_inscriptions_request_l1_batch_number_fkey FOREIGN KEY(l1_batch_number) REFERENCES l1_batches(number) ON DELETE CASCADE;
ALTER TABLE via_l1_batch_inscription_request ADD CONSTRAINT via_l1_batch_inscription_request_l1_batch_number_fkey FOREIGN KEY(l1_batch_number) REFERENCES l1_batches(number) ON DELETE CASCADE;
ALTER TABLE via_votes ADD CONSTRAINT via_votes_l1_batch_number_fkey FOREIGN KEY(l1_batch_number) REFERENCES l1_batches(number) ON DELETE CASCADE;
DROP INDEX via_priority_operation_identity;
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
