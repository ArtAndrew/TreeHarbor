CREATE TABLE th_source_profiles (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    digest bytea NOT NULL CHECK (octet_length(digest)=32),
    definition bytea NOT NULL CHECK (octet_length(definition) BETWEEN 26 AND 65536),
    principal text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,repository,digest),
    FOREIGN KEY (tenant,repository) REFERENCES th_repositories(tenant,repository)
);
CREATE TRIGGER th_source_profile_immutable BEFORE UPDATE ON th_source_profiles
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

-- Old digests did not have persisted definitions. Do not invent a full profile
-- for them or rewrite historical command receipts. New creation binds both.
ALTER TABLE th_workspaces ADD COLUMN resolved_profile bytea;
ALTER TABLE th_workspaces ADD CONSTRAINT th_workspace_profile_binding
    CHECK (resolved_profile IS NULL OR resolved_profile=profile);
ALTER TABLE th_workspaces ADD CONSTRAINT th_workspace_profile_definition
    FOREIGN KEY (tenant,repository,resolved_profile)
    REFERENCES th_source_profiles(tenant,repository,digest);
