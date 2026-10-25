CREATE TABLE th_github_managed_checks (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    check_run uuid NOT NULL,
    epoch bigint NOT NULL CHECK(epoch>0),
    queue uuid NOT NULL,
    membership_sequence bigint NOT NULL,
    import uuid NOT NULL,
    revision text NOT NULL,
    snapshot bytea NOT NULL CHECK(octet_length(snapshot)=32),
    subject jsonb NOT NULL,
    program bytea NOT NULL,
    validator text NOT NULL,
    principal text NOT NULL,
    token uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,check_run),
    FOREIGN KEY(tenant,repository,queue,membership_sequence)
        REFERENCES th_github_queue_reports(tenant,repository,queue,sequence),
    FOREIGN KEY(tenant,repository,import) REFERENCES th_github_imports(tenant,repository,import),
    FOREIGN KEY(tenant,repository,program) REFERENCES th_check_programs(tenant,repository,digest),
    FOREIGN KEY(tenant,repository,revision) REFERENCES th_bases(tenant,repository,revision),
    FOREIGN KEY(tenant,token) REFERENCES th_access_tokens(tenant,token)
);
CREATE TRIGGER th_github_managed_check_immutable BEFORE UPDATE OR DELETE ON th_github_managed_checks
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_github_managed_check_revocations (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    check_run uuid NOT NULL,
    principal text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,check_run),
    FOREIGN KEY(tenant,repository,check_run) REFERENCES th_github_managed_checks(tenant,repository,check_run)
);
CREATE TRIGGER th_github_managed_check_revocation_immutable BEFORE UPDATE OR DELETE ON th_github_managed_check_revocations
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_github_managed_executions (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    check_run uuid NOT NULL,
    token uuid NOT NULL,
    workspace uuid NOT NULL,
    execution_run uuid NOT NULL,
    state text NOT NULL CHECK(state IN ('preparing','requested','collected')),
    outcome text CHECK(outcome IN ('passed','failed','cancelled','error')),
    evidence bytea CHECK(octet_length(evidence)=32),
    definition bytea CHECK(octet_length(definition) BETWEEN 1 AND 65536),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,check_run),
    UNIQUE(tenant,repository,workspace),
    UNIQUE(tenant,repository,execution_run),
    FOREIGN KEY(tenant,repository,check_run) REFERENCES th_github_managed_checks(tenant,repository,check_run),
    FOREIGN KEY(tenant,repository,workspace) REFERENCES th_workspaces(tenant,repository,workspace),
    FOREIGN KEY(tenant,token) REFERENCES th_access_tokens(tenant,token),
    CHECK((state='collected')=(outcome IS NOT NULL AND evidence IS NOT NULL AND definition IS NOT NULL)),
    CHECK(state='collected' OR (outcome IS NULL AND evidence IS NULL AND definition IS NULL))
);
CREATE FUNCTION th_github_managed_execution_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (to_jsonb(NEW)-ARRAY['state','outcome','evidence','definition']) IS DISTINCT FROM
       (to_jsonb(OLD)-ARRAY['state','outcome','evidence','definition']) OR
       (OLD.state='collected' AND NEW IS DISTINCT FROM OLD) OR
       NOT (NEW.state=OLD.state OR
            (OLD.state='preparing' AND NEW.state='requested') OR
            (OLD.state='requested' AND NEW.state='collected')) THEN
        RAISE EXCEPTION 'check execution identity is immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_managed_execution_identity_guard BEFORE UPDATE ON th_github_managed_executions
    FOR EACH ROW EXECUTE FUNCTION th_github_managed_execution_identity();
CREATE TRIGGER th_github_managed_execution_no_delete BEFORE DELETE ON th_github_managed_executions
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
