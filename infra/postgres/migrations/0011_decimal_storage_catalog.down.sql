UPDATE billing_price_catalog
SET storage_bytes = 1073741824,
    updated_at = now()
WHERE tier IN ('free', 'pay_as_you_go')
  AND storage_bytes = 1000000000;

UPDATE billing_price_catalog
SET storage_bytes = 10737418240,
    updated_at = now()
WHERE tier = 'pro'
  AND storage_bytes = 10000000000;
