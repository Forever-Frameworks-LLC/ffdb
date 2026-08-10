ALTER TABLE project_auth_settings
    ADD COLUMN allowed_web_origins text[] NOT NULL DEFAULT '{}',
    ADD COLUMN allowed_auth_redirects text[] NOT NULL DEFAULT '{}';

ALTER TABLE project_auth_settings
    ADD CONSTRAINT project_auth_settings_web_origins_bounded
        CHECK (cardinality(allowed_web_origins) <= 20),
    ADD CONSTRAINT project_auth_settings_auth_redirects_bounded
        CHECK (cardinality(allowed_auth_redirects) <= 20);
