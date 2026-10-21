CREATE TABLE th_principals (
    tenant text NOT NULL CHECK (tenant ~ '^[a-z0-9-]{1,64}$'),
    principal text NOT NULL CHECK (length(principal) BETWEEN 1 AND 128),
    kind text NOT NULL CHECK (kind IN ('human','agent','node','validator','bridge')),
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,principal)
);
CREATE TABLE th_repository_access (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    principal text NOT NULL,
    permissions text[] NOT NULL CHECK (permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.publish']::text[]),
    PRIMARY KEY (tenant,repository,principal),
    FOREIGN KEY (tenant,repository) REFERENCES th_repositories(tenant,repository),
    FOREIGN KEY (tenant,principal) REFERENCES th_principals(tenant,principal)
);
CREATE TABLE th_access_tokens (
    tenant text NOT NULL,
    token uuid NOT NULL,
    repository uuid NOT NULL,
    principal text NOT NULL,
    fingerprint bytea NOT NULL UNIQUE CHECK (octet_length(fingerprint)=32),
    permissions text[] NOT NULL CHECK (cardinality(permissions)>0 AND permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.publish']::text[]),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,token),
    FOREIGN KEY (tenant,repository,principal) REFERENCES th_repository_access(tenant,repository,principal)
);
CREATE INDEX th_access_tokens_principal ON th_access_tokens(tenant,principal);
CREATE TABLE th_security_audit (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant text NOT NULL,
    repository uuid,
    operator text NOT NULL,
    action text NOT NULL,
    principal text NOT NULL,
    token uuid,
    permissions text[],
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE FUNCTION th_security_audit_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'security audit is append-only';
END;
$$;
CREATE TRIGGER th_security_audit_no_changes BEFORE UPDATE OR DELETE ON th_security_audit
    FOR EACH ROW EXECUTE FUNCTION th_security_audit_immutable();
