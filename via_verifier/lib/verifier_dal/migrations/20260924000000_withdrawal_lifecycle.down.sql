-- Re-enabling old writers or deleting reservations could authorize a second payment.
DO $$ BEGIN
    RAISE EXCEPTION 'withdrawal lifecycle rollback requires explicit offline reconciliation; durable signing evidence must be preserved';
END; $$;
