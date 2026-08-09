-- Authentication buckets are transient admission state. Map them back to the
-- legacy dimension names so rollback preserves state without violating the
-- original constraint.
UPDATE rate_limit_state
SET dimension = CASE dimension
    WHEN 'auth_project' THEN 'project'
    WHEN 'auth_user' THEN 'user'
    WHEN 'auth_api_key' THEN 'api_key'
    ELSE dimension
END
WHERE dimension IN ('auth_project', 'auth_user', 'auth_api_key');

ALTER TABLE rate_limit_state
    DROP CONSTRAINT rate_limit_state_dimension_check;

ALTER TABLE rate_limit_state
    ADD CONSTRAINT rate_limit_state_dimension_check
    CHECK (dimension IN ('ip', 'project', 'user', 'api_key'));
