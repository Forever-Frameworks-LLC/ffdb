-- Deployments created before instance onboarding already have platform users but
-- no instance owner. Adopt the earliest enabled user (falling back to the
-- earliest user) so the authenticated owner can finish the new setup wizard.
ALTER TABLE instance_settings
ADD COLUMN adopted_legacy_owner boolean NOT NULL DEFAULT false;

WITH candidate AS (
    SELECT id
    FROM platform_users
    ORDER BY (disabled_at IS NOT NULL), created_at, id
    LIMIT 1
)
INSERT INTO instance_settings
    (singleton, owner_user_id, deployment_mode, organization_creation_policy,
     billing_enforcement_enabled, setup_completed_at, adopted_legacy_owner)
SELECT true, id, 'unconfigured', 'owner_only', false, NULL, true
FROM candidate
WHERE NOT EXISTS (SELECT 1 FROM instance_settings WHERE singleton = true);

INSERT INTO instance_administrators (user_id, role, granted_by)
SELECT owner_user_id, 'owner', NULL
FROM instance_settings
WHERE singleton = true
  AND NOT EXISTS (SELECT 1 FROM instance_administrators WHERE role = 'owner')
ON CONFLICT (user_id) DO NOTHING;
