-- Preserve the checksum of the already-released 0006 migration and normalize
-- its legacy binary-gigabyte defaults through a forward-only catalog change.
-- Exact-value predicates leave operator-customized allowances untouched.
UPDATE billing_price_catalog
SET storage_bytes = 1000000000,
    updated_at = now()
WHERE tier IN ('free', 'pay_as_you_go')
  AND storage_bytes = 1073741824;

UPDATE billing_price_catalog
SET storage_bytes = 10000000000,
    updated_at = now()
WHERE tier = 'pro'
  AND storage_bytes = 10737418240;
