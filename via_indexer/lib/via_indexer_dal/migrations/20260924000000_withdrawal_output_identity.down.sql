-- Reverting to reference identity would discard independent payment evidence.
DO $$ BEGIN
    RAISE EXCEPTION 'Withdrawal output identity migration requires an explicit evidence-preserving rollback';
END $$;
