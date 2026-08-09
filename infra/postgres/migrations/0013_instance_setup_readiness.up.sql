-- Paid instance onboarding is not complete until Stripe can accept charges and
-- the FFDB catalog is bound. Connect credentials supplied by the owner are
-- encrypted by the API and stored as instance-scoped secret envelopes.

DO $$
DECLARE
    constraint_name text;
BEGIN
    SELECT conname INTO constraint_name
      FROM pg_constraint
     WHERE conrelid = 'instance_settings'::regclass
       AND contype = 'c'
       AND pg_get_constraintdef(oid) LIKE '%deployment_mode%unconfigured%setup_completed_at%'
     LIMIT 1;
    IF constraint_name IS NOT NULL THEN
        EXECUTE format('ALTER TABLE instance_settings DROP CONSTRAINT %I', constraint_name);
    END IF;
END $$;

ALTER TABLE instance_settings
    ADD CONSTRAINT instance_setup_completion_valid CHECK (
        deployment_mode <> 'unconfigured' OR setup_completed_at IS NULL
    );

DO $$
DECLARE
    constraint_name text;
BEGIN
    SELECT conname INTO constraint_name
      FROM pg_constraint
     WHERE conrelid = 'instance_billing_secrets'::regclass
       AND contype = 'c'
       AND pg_get_constraintdef(oid) LIKE '%secret_kind%stripe_connect_access_token%'
     LIMIT 1;
    IF constraint_name IS NOT NULL THEN
        EXECUTE format('ALTER TABLE instance_billing_secrets DROP CONSTRAINT %I', constraint_name);
    END IF;
END $$;

ALTER TABLE instance_billing_secrets
    ADD CONSTRAINT instance_billing_secrets_kind_valid CHECK (secret_kind IN (
        'stripe_secret_key',
        'stripe_webhook_secret',
        'stripe_connect_access_token',
        'stripe_connect_secret_key',
        'stripe_connect_webhook_secret'
    ));
