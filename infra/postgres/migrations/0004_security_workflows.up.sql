ALTER TABLE idempotency_keys DROP CONSTRAINT idempotency_keys_pkey;
ALTER TABLE idempotency_keys ALTER COLUMN project_id DROP NOT NULL;
ALTER TABLE idempotency_keys ADD COLUMN organization_id uuid
    REFERENCES organizations(id) ON DELETE RESTRICT;
ALTER TABLE idempotency_keys ADD COLUMN owner_token uuid;
ALTER TABLE idempotency_keys ADD COLUMN lease_expires_at timestamptz;
ALTER TABLE idempotency_keys ADD COLUMN completed_at timestamptz;
ALTER TABLE idempotency_keys ADD CONSTRAINT idempotency_keys_exactly_one_scope
    CHECK (((project_id IS NOT NULL)::integer +
            (organization_id IS NOT NULL)::integer) = 1);
ALTER TABLE idempotency_keys ADD CONSTRAINT idempotency_keys_response_bounded
    CHECK (response_body IS NULL OR octet_length(response_body::text) <= 524288);
CREATE UNIQUE INDEX idempotency_keys_project_unique
    ON idempotency_keys(project_id, operation, key_hash)
    WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX idempotency_keys_organization_unique
    ON idempotency_keys(organization_id, operation, key_hash)
    WHERE organization_id IS NOT NULL;
CREATE INDEX idempotency_keys_expiry ON idempotency_keys(expires_at);

CREATE TABLE organization_invitations (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    normalized_email text NOT NULL,
    role text NOT NULL CHECK (role IN ('owner', 'admin', 'developer', 'viewer')),
    lookup_prefix text NOT NULL UNIQUE,
    keyed_hash bytea NOT NULL CHECK (octet_length(keyed_hash) = 32),
    invited_by uuid NOT NULL REFERENCES platform_users(id) ON DELETE RESTRICT,
    expires_at timestamptz NOT NULL,
    accepted_at timestamptz,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (expires_at > created_at)
);
CREATE UNIQUE INDEX organization_invitations_active_email
    ON organization_invitations(organization_id, lower(normalized_email))
    WHERE accepted_at IS NULL AND revoked_at IS NULL;
CREATE INDEX organization_invitations_expiry ON organization_invitations(expires_at)
    WHERE accepted_at IS NULL AND revoked_at IS NULL;

ALTER TABLE email_delivery_jobs ALTER COLUMN project_id DROP NOT NULL;
ALTER TABLE email_delivery_jobs ADD COLUMN organization_id uuid
    REFERENCES organizations(id) ON DELETE CASCADE;
ALTER TABLE email_delivery_jobs ADD CONSTRAINT email_delivery_jobs_exactly_one_tenant
    CHECK (((project_id IS NOT NULL)::integer +
            (organization_id IS NOT NULL)::integer) = 1);
ALTER TABLE email_delivery_jobs
    DROP CONSTRAINT email_delivery_jobs_project_id_idempotency_key_key;
CREATE UNIQUE INDEX email_delivery_jobs_project_idempotency
    ON email_delivery_jobs(project_id, idempotency_key)
    WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX email_delivery_jobs_organization_idempotency
    ON email_delivery_jobs(organization_id, idempotency_key)
    WHERE organization_id IS NOT NULL;
