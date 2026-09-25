-- Dropping a development designation would let a strict verifier later read its unproven approvals as verified.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM via_verifier_store_mode WHERE proof_verification_dev_mode) THEN
        RAISE EXCEPTION 'store is designated for proof verification development mode; discard it instead of rolling back';
    END IF;
END $$;

ALTER TABLE via_votable_transactions DROP COLUMN IF EXISTS unverified_dev;
DROP TABLE IF EXISTS via_verifier_store_mode;
