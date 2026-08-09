CREATE TABLE project_auth_settings (
    project_id uuid PRIMARY KEY REFERENCES projects(id) ON DELETE RESTRICT,
    registration_enabled boolean NOT NULL DEFAULT true,
    email_verification_required boolean NOT NULL DEFAULT true,
    access_token_ttl_seconds integer NOT NULL DEFAULT 900
        CHECK (access_token_ttl_seconds BETWEEN 60 AND 900),
    refresh_token_ttl_seconds integer NOT NULL DEFAULT 2592000
        CHECK (refresh_token_ttl_seconds BETWEEN 3600 AND 7776000),
    password_min_length integer NOT NULL DEFAULT 8
        CHECK (password_min_length BETWEEN 8 AND 128),
    updated_by uuid REFERENCES platform_users(id) ON DELETE RESTRICT,
    updated_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO project_auth_settings (project_id)
SELECT id FROM projects
ON CONFLICT (project_id) DO NOTHING;

CREATE FUNCTION ffdb_create_project_auth_settings() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO project_auth_settings (project_id)
    VALUES (NEW.id);
    RETURN NEW;
END;
$$;

CREATE TRIGGER projects_create_auth_settings
    AFTER INSERT ON projects
    FOR EACH ROW EXECUTE FUNCTION ffdb_create_project_auth_settings();
