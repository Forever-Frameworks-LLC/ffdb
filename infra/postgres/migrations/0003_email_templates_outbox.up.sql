CREATE TABLE email_template_versions (
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('verification', 'password_reset', 'email_change', 'invitation', 'magic_link')),
    version bigint NOT NULL CHECK (version > 0),
    source text NOT NULL CHECK (octet_length(source) <= 1000000),
    source_sha256 text NOT NULL CHECK (source_sha256 ~ '^[0-9a-f]{64}$'),
    subject_template text NOT NULL CHECK (char_length(subject_template) BETWEEN 1 AND 998),
    html_template text NOT NULL CHECK (octet_length(html_template) BETWEEN 1 AND 1000000),
    text_template text NOT NULL CHECK (octet_length(text_template) <= 500000),
    allowed_variables jsonb NOT NULL CHECK (jsonb_typeof(allowed_variables) = 'array'),
    compilation_errors jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(compilation_errors) = 'array'),
    artifact_status text NOT NULL DEFAULT 'validated' CHECK (artifact_status IN ('validated', 'rejected')),
    compiled_at timestamptz,
    published_at timestamptz,
    created_by uuid NOT NULL REFERENCES api_keys(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, kind, version)
);

CREATE UNIQUE INDEX email_template_one_published_kind
    ON email_template_versions(project_id, kind)
    WHERE published_at IS NOT NULL;

CREATE TABLE email_template_compilation_jobs (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('verification', 'password_reset', 'email_change', 'invitation', 'magic_link')),
    version bigint NOT NULL CHECK (version > 0),
    state text NOT NULL CHECK (state IN ('queued', 'validated', 'failed')),
    source_sha256 text NOT NULL CHECK (source_sha256 ~ '^[0-9a-f]{64}$'),
    errors jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(errors) = 'array'),
    created_by uuid NOT NULL REFERENCES api_keys(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz
);

CREATE INDEX email_template_compilation_jobs_project_created
    ON email_template_compilation_jobs(project_id, created_at DESC);

CREATE TABLE email_delivery_jobs (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('verification', 'password_reset', 'email_change', 'invitation', 'magic_link')),
    template_version bigint NOT NULL CHECK (template_version > 0),
    recipient_fingerprint bytea NOT NULL CHECK (octet_length(recipient_fingerprint) = 32),
    encrypted_message bytea NOT NULL CHECK (octet_length(encrypted_message) BETWEEN 29 AND 2200000),
    encryption_key_version integer NOT NULL CHECK (encryption_key_version > 0),
    idempotency_key text NOT NULL CHECK (char_length(idempotency_key) BETWEEN 8 AND 256),
    state text NOT NULL CHECK (state IN ('queued', 'processing', 'delivered', 'dead')),
    attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    max_attempts integer NOT NULL DEFAULT 8 CHECK (max_attempts BETWEEN 1 AND 32),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    locked_at timestamptz,
    provider_message_id text CHECK (provider_message_id IS NULL OR char_length(provider_message_id) <= 256),
    last_error_code text CHECK (last_error_code IS NULL OR char_length(last_error_code) <= 128),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    delivered_at timestamptz,
    UNIQUE (project_id, idempotency_key)
);

CREATE INDEX email_delivery_jobs_due
    ON email_delivery_jobs(next_attempt_at, created_at)
    WHERE state IN ('queued', 'processing');

CREATE INDEX email_delivery_jobs_project_created
    ON email_delivery_jobs(project_id, created_at DESC);
