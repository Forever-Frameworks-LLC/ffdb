-- Keep durable admission-control tables bounded without storing raw caller
-- identifiers. The singleton counter is maintained transactionally by triggers,
-- so new-key admission can serialize on one row instead of scanning the table.
CREATE INDEX rate_limit_state_expiry_idx ON rate_limit_state(expires_at);

CREATE TABLE rate_limit_capacity (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    entry_count bigint NOT NULL CHECK (entry_count >= 0)
);
INSERT INTO rate_limit_capacity (singleton,entry_count)
    SELECT true,count(*) FROM rate_limit_state;

CREATE FUNCTION ffdb_account_rate_limit_state() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE rate_limit_capacity SET entry_count=entry_count+1 WHERE singleton=true;
        RETURN NEW;
    END IF;
    UPDATE rate_limit_capacity SET entry_count=GREATEST(entry_count-1,0) WHERE singleton=true;
    RETURN OLD;
END;
$$;
CREATE TRIGGER rate_limit_state_count_insert
    AFTER INSERT ON rate_limit_state
    FOR EACH ROW EXECUTE FUNCTION ffdb_account_rate_limit_state();
CREATE TRIGGER rate_limit_state_count_delete
    AFTER DELETE ON rate_limit_state
    FOR EACH ROW EXECUTE FUNCTION ffdb_account_rate_limit_state();

CREATE INDEX idempotency_keys_purge_idx
    ON idempotency_keys(expires_at,lease_expires_at);

-- A restore whose response is lost remains fenced to its original durable
-- worker receipt. A retry with another idempotency key cannot replay it.
ALTER TABLE backups ADD COLUMN restore_receipt_id uuid;
CREATE UNIQUE INDEX backups_restore_receipt_unique
    ON backups(restore_receipt_id) WHERE restore_receipt_id IS NOT NULL;
