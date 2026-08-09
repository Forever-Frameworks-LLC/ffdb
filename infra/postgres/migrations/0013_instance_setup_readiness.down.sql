ALTER TABLE instance_settings
    DROP CONSTRAINT IF EXISTS instance_setup_completion_valid;

-- The earlier schema could not represent an in-progress configured mode.
UPDATE instance_settings
   SET setup_completed_at = now(), updated_at = now()
 WHERE deployment_mode <> 'unconfigured' AND setup_completed_at IS NULL;

ALTER TABLE instance_settings
    ADD CONSTRAINT instance_settings_setup_completion_check CHECK (
        (deployment_mode = 'unconfigured') = (setup_completed_at IS NULL)
    );

ALTER TABLE instance_billing_secrets
    DROP CONSTRAINT IF EXISTS instance_billing_secrets_kind_valid;

DELETE FROM instance_billing_secrets
 WHERE secret_kind IN ('stripe_connect_secret_key','stripe_connect_webhook_secret');

ALTER TABLE instance_billing_secrets
    ADD CONSTRAINT instance_billing_secrets_kind_check CHECK (secret_kind IN (
        'stripe_secret_key',
        'stripe_webhook_secret',
        'stripe_connect_access_token'
    ));
