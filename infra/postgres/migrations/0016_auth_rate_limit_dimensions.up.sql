ALTER TABLE rate_limit_state
    DROP CONSTRAINT rate_limit_state_dimension_check;

ALTER TABLE rate_limit_state
    ADD CONSTRAINT rate_limit_state_dimension_check
    CHECK (
        dimension IN (
            'ip',
            'auth_project',
            'auth_user',
            'auth_api_key',
            'project',
            'user',
            'api_key'
        )
    );
