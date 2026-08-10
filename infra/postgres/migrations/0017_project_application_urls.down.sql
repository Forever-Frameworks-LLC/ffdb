ALTER TABLE project_auth_settings
    DROP CONSTRAINT project_auth_settings_auth_redirects_bounded,
    DROP CONSTRAINT project_auth_settings_web_origins_bounded,
    DROP COLUMN allowed_auth_redirects,
    DROP COLUMN allowed_web_origins;
