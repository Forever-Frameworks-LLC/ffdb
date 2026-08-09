-- Usage billing control-plane state. Fine-grained usage and the Stripe outbox
-- live in one isolated SQLite ledger per organization; PostgreSQL owns provider
-- identity, entitlement periods, invoice lifecycle, and operator-visible health.

ALTER TABLE organization_billing_accounts
    ADD COLUMN current_period_start timestamptz,
    ADD COLUMN usage_reporting_status text NOT NULL DEFAULT 'healthy'
        CHECK (usage_reporting_status IN ('healthy','degraded','blocked','reconciling')),
    ADD COLUMN usage_reporting_last_success_at timestamptz,
    ADD COLUMN usage_reporting_hard_cutoff_at timestamptz;

CREATE TABLE organization_billing_invoices (
    organization_id uuid NOT NULL REFERENCES organizations(id) ON DELETE RESTRICT,
    provider text NOT NULL CHECK (provider = 'stripe'),
    provider_invoice_id text NOT NULL,
    provider_subscription_id text,
    status text NOT NULL CHECK (status IN
        ('draft','open','paid','uncollectible','void','payment_failed')),
    currency text NOT NULL CHECK (currency ~ '^[a-z]{3}$'),
    amount_due_cents bigint NOT NULL CHECK (amount_due_cents >= 0),
    amount_paid_cents bigint NOT NULL CHECK (amount_paid_cents >= 0),
    period_start timestamptz,
    period_end timestamptz,
    hosted_invoice_url text,
    invoice_pdf_url text,
    provider_created_at timestamptz NOT NULL,
    last_provider_event_created_at timestamptz NOT NULL,
    last_provider_event_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, provider_invoice_id),
    CHECK (amount_paid_cents <= amount_due_cents OR status = 'paid'),
    CHECK (hosted_invoice_url IS NULL OR hosted_invoice_url ~ '^https://'),
    CHECK (invoice_pdf_url IS NULL OR invoice_pdf_url ~ '^https://')
);
CREATE INDEX organization_billing_invoices_org_created_idx
    ON organization_billing_invoices(organization_id, provider_created_at DESC);

CREATE TABLE billing_usage_catalog (
    metric text PRIMARY KEY CHECK (metric IN
        ('reads','writes','storage_byte_hours','monthly_active_users')),
    display_name text NOT NULL,
    event_name text NOT NULL UNIQUE CHECK (event_name ~ '^[a-z0-9_]{3,100}$'),
    provider_meter_id text NOT NULL UNIQUE CHECK (provider_meter_id ~ '^mtr_[A-Za-z0-9_]+$'),
    payg_price_id text NOT NULL UNIQUE CHECK (payg_price_id ~ '^price_[A-Za-z0-9_]+$'),
    pro_price_id text NOT NULL UNIQUE CHECK (pro_price_id ~ '^price_[A-Za-z0-9_]+$'),
    aggregation text NOT NULL CHECK (aggregation IN ('sum','last')),
    unit_name text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL DEFAULT now()
);
