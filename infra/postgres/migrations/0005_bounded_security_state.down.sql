DROP INDEX IF EXISTS backups_restore_receipt_unique;
ALTER TABLE backups DROP COLUMN IF EXISTS restore_receipt_id;
DROP INDEX IF EXISTS idempotency_keys_purge_idx;
DROP TRIGGER IF EXISTS rate_limit_state_count_delete ON rate_limit_state;
DROP TRIGGER IF EXISTS rate_limit_state_count_insert ON rate_limit_state;
DROP FUNCTION IF EXISTS ffdb_account_rate_limit_state();
DROP TABLE IF EXISTS rate_limit_capacity;
DROP INDEX IF EXISTS rate_limit_state_expiry_idx;
