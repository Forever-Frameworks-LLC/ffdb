-- Platform billing is organization-scoped. Project commerce is intentionally a
-- separate table and identifier namespace so an FFDB subscription can never be
-- confused with a customer's shop/payment account.

CREATE TABLE billing_price_catalog (
    tier text PRIMARY KEY CHECK (tier IN ('free', 'pay_as_you_go', 'pro')),
    display_name text NOT NULL,
    billing_unit text NOT NULL CHECK (billing_unit IN ('organization', 'seat')),
    base_price_cents integer CHECK (base_price_cents IS NULL OR base_price_cents >= 0),
    currency text NOT NULL DEFAULT 'usd' CHECK (currency ~ '^[a-z]{3}$'),
    project_limit integer CHECK (project_limit IS NULL OR project_limit > 0),
    storage_bytes bigint NOT NULL CHECK (storage_bytes > 0),
    monthly_reads bigint NOT NULL CHECK (monthly_reads > 0),
    monthly_writes bigint NOT NULL CHECK (monthly_writes > 0),
    monthly_active_users bigint NOT NULL CHECK (monthly_active_users > 0),
    overage_enabled boolean NOT NULL,
    reads_at_limit text NOT NULL CHECK (reads_at_limit IN ('continue', 'overage')),
    writes_at_limit text NOT NULL CHECK (writes_at_limit IN ('pause', 'overage')),
    signups_at_limit text NOT NULL CHECK (signups_at_limit IN ('pause', 'overage')),
    requires_payment_method_for_overage boolean NOT NULL DEFAULT true,
    active boolean NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO billing_price_catalog
    (tier,display_name,billing_unit,base_price_cents,currency,project_limit,
     storage_bytes,monthly_reads,monthly_writes,monthly_active_users,overage_enabled,
     reads_at_limit,writes_at_limit,signups_at_limit,requires_payment_method_for_overage)
VALUES
    ('free','Free','organization',0,'usd',2,1073741824,1000000,50000,5000,false,
     'continue','pause','pause',true),
    ('pay_as_you_go','Pay as you go','organization',NULL,'usd',NULL,1073741824,1000000,50000,5000,true,
     'overage','overage','overage',true),
    ('pro','Pro','organization',700,'usd',NULL,10737418240,15000000,750000,50000,true,
     'overage','overage','overage',true);

CREATE TABLE organization_billing_accounts (
    organization_id uuid PRIMARY KEY REFERENCES organizations(id) ON DELETE RESTRICT,
    provider text NOT NULL CHECK (provider = 'stripe'),
    provider_customer_id text NOT NULL,
    provider_subscription_id text,
    tier text NOT NULL REFERENCES billing_price_catalog(tier) CHECK (tier <> 'free'),
    status text NOT NULL CHECK (status IN
        ('checkout_pending','trialing','active','past_due','unpaid','canceled','paused','incomplete')),
    billing_unit text NOT NULL CHECK (billing_unit IN ('organization', 'seat')),
    seat_quantity integer NOT NULL DEFAULT 1 CHECK (seat_quantity > 0),
    current_period_end timestamptz,
    cancel_at_period_end boolean NOT NULL DEFAULT false,
    last_provider_event_created_at timestamptz NOT NULL,
    last_provider_event_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider, provider_customer_id),
    UNIQUE (provider, provider_subscription_id)
);

CREATE TABLE billing_webhook_events (
    provider text NOT NULL CHECK (provider = 'stripe'),
    provider_event_id text NOT NULL,
    event_type text NOT NULL,
    livemode boolean NOT NULL,
    payload_sha256 bytea NOT NULL CHECK (octet_length(payload_sha256) = 32),
    organization_id uuid REFERENCES organizations(id) ON DELETE RESTRICT,
    provider_created_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, provider_event_id)
);
CREATE INDEX billing_webhook_events_received_idx
    ON billing_webhook_events(received_at);

CREATE TABLE project_commerce_accounts (
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    provider text NOT NULL,
    provider_account_id text NOT NULL,
    status text NOT NULL CHECK (status IN ('enabled', 'restricted')),
    charge_model text NOT NULL CHECK (charge_model IN ('destination', 'direct')),
    capabilities text[] NOT NULL DEFAULT '{}',
    -- Provider-specific responsibility, dashboard, and requirement-collection
    -- settings. This must not contain secret keys.
    controller_configuration jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, provider),
    UNIQUE (project_id),
    UNIQUE (provider, provider_account_id),
    CHECK (provider <> '' AND provider_account_id <> '')
);
