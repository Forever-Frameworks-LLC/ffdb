DROP TRIGGER project_commerce_accounts_rebind_guard ON project_commerce_accounts;
DROP FUNCTION guard_project_commerce_account_rebind();
DROP TABLE commerce_fulfillment_events;
DROP TABLE commerce_webhook_events;
DROP TABLE commerce_entitlements;
DROP TABLE commerce_refunds;
DROP TABLE commerce_payments;
DROP TABLE commerce_subscriptions;
DROP TABLE commerce_order_lines;
DROP TABLE commerce_orders;
DROP TABLE commerce_customers;
DROP TABLE commerce_prices;
DROP TABLE commerce_products;
DROP TABLE project_commerce_secrets;
ALTER TABLE project_commerce_accounts
    DROP CONSTRAINT IF EXISTS project_commerce_accounts_mode_binding_check,
    DROP CONSTRAINT IF EXISTS project_commerce_accounts_provider_check,
    DROP CONSTRAINT IF EXISTS project_commerce_accounts_charge_model_check,
    DROP CONSTRAINT IF EXISTS project_commerce_accounts_status_check,
    DROP COLUMN disabled_reason,
    DROP COLUMN requirements_due,
    DROP COLUMN onboarding_url_expires_at,
    DROP COLUMN livemode,
    DROP COLUMN mode,
    ALTER COLUMN provider_account_id SET NOT NULL,
    ADD CONSTRAINT project_commerce_accounts_status_check CHECK (status IN ('enabled','restricted')),
    ADD CONSTRAINT project_commerce_accounts_charge_model_check CHECK (charge_model IN ('destination','direct'));
