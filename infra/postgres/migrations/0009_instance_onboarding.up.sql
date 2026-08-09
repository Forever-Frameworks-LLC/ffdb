-- Instance ownership and deployment policy are durable control-plane state.
-- The first platform user is installed as owner in the same transaction that
-- creates that user; all later changes require an authenticated instance admin.

CREATE TABLE instance_settings (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    owner_user_id uuid NOT NULL UNIQUE REFERENCES platform_users(id) ON DELETE RESTRICT,
    deployment_mode text NOT NULL DEFAULT 'unconfigured' CHECK (deployment_mode IN
        ('unconfigured','private','team','platform_byo','platform_connect')),
    organization_creation_policy text NOT NULL DEFAULT 'owner_only' CHECK
        (organization_creation_policy IN ('owner_only','authenticated','invitation_only')),
    billing_enforcement_enabled boolean NOT NULL DEFAULT false,
    setup_completed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((deployment_mode = 'unconfigured') = (setup_completed_at IS NULL)),
    CHECK (billing_enforcement_enabled = (deployment_mode IN ('platform_byo','platform_connect')))
);

CREATE TABLE instance_administrators (
    user_id uuid PRIMARY KEY REFERENCES platform_users(id) ON DELETE RESTRICT,
    role text NOT NULL CHECK (role IN ('owner','admin')),
    granted_by uuid REFERENCES platform_users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((role = 'owner') = (granted_by IS NULL))
);
CREATE UNIQUE INDEX instance_single_owner_idx ON instance_administrators(role) WHERE role='owner';

CREATE TABLE instance_billing_accounts (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    provider text NOT NULL DEFAULT 'stripe' CHECK (provider='stripe'),
    mode text NOT NULL CHECK (mode IN ('byo_keys','stripe_connect')),
    provider_account_id text,
    status text NOT NULL CHECK (status IN
        ('pending','onboarding','enabled','restricted','disconnected')),
    charges_enabled boolean NOT NULL DEFAULT false,
    payouts_enabled boolean NOT NULL DEFAULT false,
    details_submitted boolean NOT NULL DEFAULT false,
    capabilities text[] NOT NULL DEFAULT '{}',
    updated_by uuid NOT NULL REFERENCES platform_users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (mode='byo_keys' AND (provider_account_id IS NULL OR provider_account_id ~ '^acct_[A-Za-z0-9]+$')) OR
        (mode='stripe_connect' AND provider_account_id ~ '^acct_[A-Za-z0-9]+$')
    )
);

CREATE TABLE instance_billing_secrets (
    secret_kind text NOT NULL CHECK (secret_kind IN
        ('stripe_secret_key','stripe_webhook_secret','stripe_connect_access_token')),
    key_version integer NOT NULL CHECK (key_version > 0),
    nonce bytea NOT NULL CHECK (octet_length(nonce)=12),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) > 16),
    updated_by uuid NOT NULL REFERENCES platform_users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (secret_kind)
);

CREATE TABLE organization_billing_exemptions (
    organization_id uuid PRIMARY KEY REFERENCES organizations(id) ON DELETE RESTRICT,
    reason text NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 500),
    created_by uuid NOT NULL REFERENCES platform_users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now()
);

