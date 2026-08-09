DROP INDEX IF EXISTS email_delivery_jobs_organization_idempotency;
DROP INDEX IF EXISTS email_delivery_jobs_project_idempotency;
DELETE FROM email_delivery_jobs WHERE organization_id IS NOT NULL;
ALTER TABLE email_delivery_jobs DROP CONSTRAINT IF EXISTS email_delivery_jobs_exactly_one_tenant;
ALTER TABLE email_delivery_jobs DROP COLUMN IF EXISTS organization_id;
ALTER TABLE email_delivery_jobs ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE email_delivery_jobs
    ADD CONSTRAINT email_delivery_jobs_project_id_idempotency_key_key
    UNIQUE (project_id, idempotency_key);

DROP TABLE IF EXISTS organization_invitations;

DROP INDEX IF EXISTS idempotency_keys_expiry;
DROP INDEX IF EXISTS idempotency_keys_organization_unique;
DROP INDEX IF EXISTS idempotency_keys_project_unique;
DELETE FROM idempotency_keys WHERE project_id IS NULL;
ALTER TABLE idempotency_keys DROP CONSTRAINT IF EXISTS idempotency_keys_response_bounded;
ALTER TABLE idempotency_keys DROP CONSTRAINT IF EXISTS idempotency_keys_exactly_one_scope;
ALTER TABLE idempotency_keys DROP COLUMN IF EXISTS completed_at;
ALTER TABLE idempotency_keys DROP COLUMN IF EXISTS lease_expires_at;
ALTER TABLE idempotency_keys DROP COLUMN IF EXISTS owner_token;
ALTER TABLE idempotency_keys DROP COLUMN IF EXISTS organization_id;
ALTER TABLE idempotency_keys ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE idempotency_keys
    ADD CONSTRAINT idempotency_keys_pkey PRIMARY KEY (project_id, operation, key_hash);
