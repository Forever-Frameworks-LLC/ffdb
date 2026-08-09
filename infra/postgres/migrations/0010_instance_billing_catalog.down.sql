DROP TABLE instance_billing_usage_catalog;
DROP TABLE instance_billing_catalog;

DELETE FROM billing_usage_catalog
WHERE provider_meter_id IS NULL OR payg_price_id IS NULL OR pro_price_id IS NULL;

ALTER TABLE billing_usage_catalog
    ALTER COLUMN provider_meter_id SET NOT NULL,
    ALTER COLUMN payg_price_id SET NOT NULL,
    ALTER COLUMN pro_price_id SET NOT NULL;
