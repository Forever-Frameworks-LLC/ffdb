-- The active Stripe catalog belongs to the instance billing account. Connect
-- accounts receive their own catalog after onboarding; IDs therefore cannot be
-- deployment-wide environment variables.

ALTER TABLE billing_usage_catalog
    ALTER COLUMN provider_meter_id DROP NOT NULL,
    ALTER COLUMN payg_price_id DROP NOT NULL,
    ALTER COLUMN pro_price_id DROP NOT NULL;

CREATE TABLE instance_billing_catalog (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    provider_account_id text NOT NULL CHECK (provider_account_id ~ '^acct_[A-Za-z0-9_]+$'),
    product_id text CHECK (product_id ~ '^prod_[A-Za-z0-9_]+$'),
    pro_base_price_id text NOT NULL CHECK (pro_base_price_id ~ '^price_[A-Za-z0-9_]+$'),
    catalog_version integer NOT NULL CHECK (catalog_version > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (singleton) REFERENCES instance_billing_accounts(singleton) ON DELETE CASCADE
);

CREATE TABLE instance_billing_usage_catalog (
    metric text PRIMARY KEY REFERENCES billing_usage_catalog(metric) ON DELETE RESTRICT,
    provider_account_id text NOT NULL CHECK (provider_account_id ~ '^acct_[A-Za-z0-9_]+$'),
    event_name text NOT NULL CHECK (event_name ~ '^[a-z0-9_]{3,100}$'),
    provider_meter_id text NOT NULL UNIQUE CHECK (provider_meter_id ~ '^mtr_[A-Za-z0-9_]+$'),
    payg_price_id text NOT NULL UNIQUE CHECK (payg_price_id ~ '^price_[A-Za-z0-9_]+$'),
    pro_price_id text NOT NULL UNIQUE CHECK (pro_price_id ~ '^price_[A-Za-z0-9_]+$'),
    catalog_version integer NOT NULL CHECK (catalog_version > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
