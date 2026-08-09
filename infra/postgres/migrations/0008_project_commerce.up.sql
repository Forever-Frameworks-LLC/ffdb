-- Complete project-commerce persistence. Provider configuration is explicitly
-- separate from FFDB platform billing. Money is always stored in integer minor
-- units and order lines retain immutable price snapshots.

ALTER TABLE project_commerce_accounts
    DROP CONSTRAINT IF EXISTS project_commerce_accounts_status_check,
    DROP CONSTRAINT IF EXISTS project_commerce_accounts_charge_model_check,
    ALTER COLUMN provider_account_id DROP NOT NULL,
    ADD COLUMN mode text NOT NULL DEFAULT 'byo_keys'
        CHECK (mode IN ('byo_keys','stripe_connect')),
    ADD COLUMN livemode boolean NOT NULL DEFAULT false,
    ADD COLUMN onboarding_url_expires_at timestamptz,
    ADD COLUMN requirements_due text[] NOT NULL DEFAULT '{}',
    ADD COLUMN disabled_reason text,
    ADD CONSTRAINT project_commerce_accounts_status_check
        CHECK (status IN ('configuring','onboarding','enabled','restricted','disconnected')),
    ADD CONSTRAINT project_commerce_accounts_charge_model_check
        CHECK (charge_model = 'direct'),
    ADD CONSTRAINT project_commerce_accounts_provider_check CHECK (provider = 'stripe'),
    ADD CONSTRAINT project_commerce_accounts_mode_binding_check CHECK (
        (mode='stripe_connect' AND provider_account_id ~ '^acct_[A-Za-z0-9_]+$') OR
        (mode='byo_keys' AND (provider_account_id IS NULL OR provider_account_id ~ '^acct_[A-Za-z0-9_]+$'))
    );

CREATE TABLE project_commerce_secrets (
    project_id uuid PRIMARY KEY REFERENCES projects(id) ON DELETE RESTRICT,
    provider text NOT NULL CHECK (provider='stripe'),
    key_version integer NOT NULL CHECK (key_version > 0),
    secret_key_ciphertext bytea NOT NULL CHECK (octet_length(secret_key_ciphertext) >= 29),
    webhook_secret_ciphertext bytea NOT NULL CHECK (octet_length(webhook_secret_ciphertext) >= 29),
    secret_key_fingerprint bytea NOT NULL CHECK (octet_length(secret_key_fingerprint)=32),
    rotated_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE commerce_products (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    description text CHECK (description IS NULL OR length(description) <= 10000),
    tax_code text CHECK (tax_code IS NULL OR length(tax_code) BETWEEN 1 AND 64),
    active boolean NOT NULL DEFAULT true,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata)='object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id,id)
);

CREATE TABLE commerce_prices (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    product_id uuid NOT NULL,
    lookup_key text CHECK (lookup_key IS NULL OR lookup_key ~ '^[A-Za-z0-9._-]{1,100}$'),
    currency text NOT NULL CHECK (currency ~ '^[a-z]{3}$'),
    unit_amount_minor bigint NOT NULL CHECK (unit_amount_minor BETWEEN 1 AND 9007199254740991),
    billing_type text NOT NULL CHECK (billing_type IN ('one_time','recurring')),
    recurring_interval text CHECK (recurring_interval IN ('day','week','month','year')),
    recurring_interval_count integer CHECK (recurring_interval_count BETWEEN 1 AND 36),
    provider_price_id text UNIQUE CHECK (provider_price_id IS NULL OR provider_price_id ~ '^price_[A-Za-z0-9_]+$'),
    entitlements jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(entitlements)='object'),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id,id),
    UNIQUE (project_id,lookup_key),
    FOREIGN KEY (project_id,product_id) REFERENCES commerce_products(project_id,id) ON DELETE RESTRICT,
    CHECK ((billing_type='one_time' AND recurring_interval IS NULL AND recurring_interval_count IS NULL) OR
           (billing_type='recurring' AND recurring_interval IS NOT NULL AND recurring_interval_count IS NOT NULL))
);

CREATE TABLE commerce_customers (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    subject_kind text NOT NULL CHECK (subject_kind IN ('individual','team','organization','guest')),
    subject_id text NOT NULL CHECK (length(subject_id) BETWEEN 1 AND 255),
    provider_customer_id text CHECK (provider_customer_id IS NULL OR provider_customer_id ~ '^cus_[A-Za-z0-9_]+$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id,id),
    UNIQUE (project_id,subject_kind,subject_id),
    UNIQUE (project_id,provider_customer_id)
);

CREATE TABLE commerce_orders (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    customer_id uuid,
    client_reference text CHECK (client_reference IS NULL OR length(client_reference) <= 255),
    status text NOT NULL CHECK (status IN
        ('pending','checkout_created','processing','paid','payment_failed','canceled','partially_refunded','refunded')),
    fulfillment_status text NOT NULL DEFAULT 'unfulfilled'
        CHECK (fulfillment_status IN ('unfulfilled','processing','fulfilled','canceled')),
    currency text NOT NULL CHECK (currency ~ '^[a-z]{3}$'),
    subtotal_minor bigint NOT NULL CHECK (subtotal_minor >= 0),
    discount_minor bigint NOT NULL DEFAULT 0 CHECK (discount_minor >= 0),
    tax_minor bigint NOT NULL DEFAULT 0 CHECK (tax_minor >= 0),
    shipping_minor bigint NOT NULL DEFAULT 0 CHECK (shipping_minor >= 0),
    total_minor bigint NOT NULL CHECK (total_minor >= 0),
    refunded_minor bigint NOT NULL DEFAULT 0 CHECK (refunded_minor >= 0),
    provider_checkout_session_id text UNIQUE CHECK (provider_checkout_session_id IS NULL OR provider_checkout_session_id ~ '^cs_[A-Za-z0-9_]+$'),
    provider_payment_intent_id text UNIQUE CHECK (provider_payment_intent_id IS NULL OR provider_payment_intent_id ~ '^pi_[A-Za-z0-9_]+$'),
    provider_charge_id text CHECK (provider_charge_id IS NULL OR provider_charge_id ~ '^ch_[A-Za-z0-9_]+$'),
    checkout_expires_at timestamptz,
    paid_at timestamptz,
    canceled_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id,id),
    FOREIGN KEY (project_id,customer_id) REFERENCES commerce_customers(project_id,id) ON DELETE RESTRICT,
    CHECK (subtotal_minor - discount_minor + tax_minor + shipping_minor = total_minor),
    CHECK (refunded_minor <= total_minor),
    CHECK (fulfillment_status <> 'fulfilled' OR status IN ('paid','partially_refunded'))
);
CREATE INDEX commerce_orders_project_created_idx ON commerce_orders(project_id,created_at DESC);

CREATE TABLE commerce_order_lines (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    order_id uuid NOT NULL,
    product_id uuid NOT NULL,
    price_id uuid NOT NULL,
    product_name text NOT NULL CHECK (length(product_name) BETWEEN 1 AND 200),
    currency text NOT NULL CHECK (currency ~ '^[a-z]{3}$'),
    unit_amount_minor bigint NOT NULL CHECK (unit_amount_minor >= 0),
    quantity integer NOT NULL CHECK (quantity BETWEEN 1 AND 1000000),
    line_total_minor bigint NOT NULL CHECK (line_total_minor >= 0),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata)='object'),
    UNIQUE (project_id,id),
    FOREIGN KEY (project_id,order_id) REFERENCES commerce_orders(project_id,id) ON DELETE RESTRICT,
    FOREIGN KEY (project_id,product_id) REFERENCES commerce_products(project_id,id) ON DELETE RESTRICT,
    FOREIGN KEY (project_id,price_id) REFERENCES commerce_prices(project_id,id) ON DELETE RESTRICT,
    CHECK (unit_amount_minor * quantity::bigint = line_total_minor)
);

CREATE TABLE commerce_payments (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    order_id uuid,
    subscription_id uuid,
    status text NOT NULL CHECK (status IN
        ('requires_payment_method','requires_action','processing','authorized','captured','partially_refunded','refunded','failed','canceled')),
    currency text NOT NULL CHECK (currency ~ '^[a-z]{3}$'),
    authorized_minor bigint NOT NULL DEFAULT 0 CHECK (authorized_minor >= 0),
    captured_minor bigint NOT NULL DEFAULT 0 CHECK (captured_minor >= 0),
    refunded_minor bigint NOT NULL DEFAULT 0 CHECK (refunded_minor >= 0),
    provider_payment_intent_id text NOT NULL UNIQUE CHECK (provider_payment_intent_id ~ '^pi_[A-Za-z0-9_]+$'),
    provider_charge_id text CHECK (provider_charge_id IS NULL OR provider_charge_id ~ '^ch_[A-Za-z0-9_]+$'),
    failure_code text,
    provider_created_at timestamptz NOT NULL,
    last_provider_event_created_at timestamptz NOT NULL,
    last_provider_event_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id,id),
    FOREIGN KEY (project_id,order_id) REFERENCES commerce_orders(project_id,id) ON DELETE RESTRICT,
    CHECK ((order_id IS NOT NULL)::int + (subscription_id IS NOT NULL)::int = 1),
    CHECK (refunded_minor <= captured_minor),
    CHECK (captured_minor <= authorized_minor OR authorized_minor=0)
);

CREATE TABLE commerce_refunds (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    order_id uuid,
    subscription_id uuid,
    payment_id uuid NOT NULL,
    status text NOT NULL CHECK (status IN ('pending','succeeded','failed','canceled')),
    amount_minor bigint NOT NULL CHECK (amount_minor > 0),
    currency text NOT NULL CHECK (currency ~ '^[a-z]{3}$'),
    reason text CHECK (reason IS NULL OR reason IN ('duplicate','fraudulent','requested_by_customer','other')),
    provider_refund_id text UNIQUE CHECK (provider_refund_id IS NULL OR provider_refund_id ~ '^re_[A-Za-z0-9_]+$'),
    failure_reason text,
    last_provider_event_created_at timestamptz,
    last_provider_event_id text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id,id),
    FOREIGN KEY (project_id,order_id) REFERENCES commerce_orders(project_id,id) ON DELETE RESTRICT,
    FOREIGN KEY (project_id,payment_id) REFERENCES commerce_payments(project_id,id) ON DELETE RESTRICT,
    CHECK ((order_id IS NOT NULL)::int + (subscription_id IS NOT NULL)::int = 1)
);

CREATE TABLE commerce_subscriptions (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    customer_id uuid NOT NULL,
    price_id uuid NOT NULL,
    subject_kind text NOT NULL CHECK (subject_kind IN ('individual','team','organization')),
    subject_id text NOT NULL CHECK (length(subject_id) BETWEEN 1 AND 255),
    quantity integer NOT NULL DEFAULT 1 CHECK (quantity BETWEEN 1 AND 1000000),
    status text NOT NULL CHECK (status IN
        ('checkout_pending','trialing','active','past_due','unpaid','paused','canceled','incomplete','expired')),
    provider_subscription_id text UNIQUE CHECK (provider_subscription_id IS NULL OR provider_subscription_id ~ '^sub_[A-Za-z0-9_]+$'),
    provider_checkout_session_id text UNIQUE CHECK (provider_checkout_session_id IS NULL OR provider_checkout_session_id ~ '^cs_[A-Za-z0-9_]+$'),
    current_period_start timestamptz,
    current_period_end timestamptz,
    cancel_at_period_end boolean NOT NULL DEFAULT false,
    last_provider_event_created_at timestamptz,
    last_provider_event_id text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id,id),
    FOREIGN KEY (project_id,customer_id) REFERENCES commerce_customers(project_id,id) ON DELETE RESTRICT,
    FOREIGN KEY (project_id,price_id) REFERENCES commerce_prices(project_id,id) ON DELETE RESTRICT
);
CREATE INDEX commerce_subscriptions_subject_idx
    ON commerce_subscriptions(project_id,subject_kind,subject_id,status);

ALTER TABLE commerce_payments ADD CONSTRAINT commerce_payments_subscription_fk
    FOREIGN KEY (project_id,subscription_id)
    REFERENCES commerce_subscriptions(project_id,id) ON DELETE RESTRICT;
ALTER TABLE commerce_refunds ADD CONSTRAINT commerce_refunds_subscription_fk
    FOREIGN KEY (project_id,subscription_id)
    REFERENCES commerce_subscriptions(project_id,id) ON DELETE RESTRICT;

CREATE TABLE commerce_entitlements (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    subscription_id uuid,
    order_id uuid,
    subject_kind text NOT NULL CHECK (subject_kind IN ('individual','team','organization')),
    subject_id text NOT NULL CHECK (length(subject_id) BETWEEN 1 AND 255),
    entitlement_key text NOT NULL CHECK (entitlement_key ~ '^[A-Za-z0-9._:-]{1,200}$'),
    entitlement_value jsonb NOT NULL CHECK (jsonb_typeof(entitlement_value)='object'),
    status text NOT NULL CHECK (status IN ('active','grace','revoked','expired')),
    valid_from timestamptz NOT NULL,
    valid_until timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id,id),
    UNIQUE (project_id,subject_kind,subject_id,entitlement_key),
    FOREIGN KEY (project_id,subscription_id) REFERENCES commerce_subscriptions(project_id,id) ON DELETE RESTRICT,
    FOREIGN KEY (project_id,order_id) REFERENCES commerce_orders(project_id,id) ON DELETE RESTRICT,
    CHECK ((subscription_id IS NOT NULL)::int + (order_id IS NOT NULL)::int = 1),
    CHECK (valid_until IS NULL OR valid_until > valid_from)
);

CREATE TABLE commerce_webhook_events (
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    provider text NOT NULL CHECK (provider='stripe'),
    provider_account_id text NOT NULL,
    provider_event_id text NOT NULL,
    event_type text NOT NULL,
    livemode boolean NOT NULL,
    payload_sha256 bytea NOT NULL CHECK (octet_length(payload_sha256)=32),
    provider_created_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz,
    processing_error text,
    PRIMARY KEY (project_id,provider,provider_event_id)
);
CREATE INDEX commerce_webhook_events_retry_idx
    ON commerce_webhook_events(project_id,received_at) WHERE processed_at IS NULL;

CREATE TABLE commerce_fulfillment_events (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    order_id uuid NOT NULL,
    state text NOT NULL CHECK (state IN ('processing','fulfilled','canceled')),
    actor_user_id uuid REFERENCES platform_users(id) ON DELETE RESTRICT,
    note text CHECK (note IS NULL OR length(note) <= 2000),
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (project_id,order_id) REFERENCES commerce_orders(project_id,id) ON DELETE RESTRICT
);

-- Provider object identifiers are account-scoped. Once a project has created
-- commerce state, rebinding it to a different Stripe account would make its
-- Product, Price, Customer, Payment and Subscription references point into the
-- wrong account. Credential rotation and mode changes remain allowed when the
-- provider account itself is unchanged.
CREATE FUNCTION guard_project_commerce_account_rebind() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF (OLD.mode, OLD.provider_account_id) IS DISTINCT FROM
       (NEW.mode, NEW.provider_account_id)
       AND OLD.provider_account_id IS DISTINCT FROM NEW.provider_account_id
       AND (
           EXISTS (SELECT 1 FROM commerce_products WHERE project_id=OLD.project_id) OR
           EXISTS (SELECT 1 FROM commerce_customers WHERE project_id=OLD.project_id) OR
           EXISTS (SELECT 1 FROM commerce_orders WHERE project_id=OLD.project_id) OR
           EXISTS (SELECT 1 FROM commerce_subscriptions WHERE project_id=OLD.project_id)
       )
    THEN
        RAISE EXCEPTION 'cannot rebind project commerce with provider-bound state'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_commerce_accounts_rebind_guard
BEFORE UPDATE OF mode, provider_account_id ON project_commerce_accounts
FOR EACH ROW EXECUTE FUNCTION guard_project_commerce_account_rebind();
