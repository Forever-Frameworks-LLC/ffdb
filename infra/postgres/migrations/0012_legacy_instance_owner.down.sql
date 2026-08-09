-- Owner adoption is intentionally retained: deleting it would strand an
-- upgraded installation between bootstrap and authenticated setup. Downgrade
-- removes only the migration marker column.
ALTER TABLE instance_settings
DROP COLUMN adopted_legacy_owner;
