DROP TABLE billing_usage_catalog;
DROP TABLE organization_billing_invoices;
ALTER TABLE organization_billing_accounts
    DROP COLUMN usage_reporting_hard_cutoff_at,
    DROP COLUMN usage_reporting_last_success_at,
    DROP COLUMN usage_reporting_status,
    DROP COLUMN current_period_start;
