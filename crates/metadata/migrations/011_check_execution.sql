CREATE TABLE th_check_programs (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    digest bytea NOT NULL CHECK(octet_length(digest)=32),
    definition bytea NOT NULL CHECK(octet_length(definition) BETWEEN 1 AND 16384),
    profile bytea NOT NULL,
    principal text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,digest),
    FOREIGN KEY(tenant,repository,profile) REFERENCES th_runtime_profiles(tenant,repository,digest)
);
CREATE TRIGGER th_check_program_immutable BEFORE UPDATE OR DELETE ON th_check_programs
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_check_executions (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    check_run uuid NOT NULL,
    candidate uuid NOT NULL,
    context bytea NOT NULL CHECK(octet_length(context)=32),
    program bytea NOT NULL,
    validator text NOT NULL,
    token uuid NOT NULL,
    workspace uuid NOT NULL,
    execution_run uuid NOT NULL,
    state text NOT NULL CHECK(state IN ('preparing','requested','collected')),
    receipt uuid,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,check_run),
    UNIQUE(tenant,repository,workspace),
    UNIQUE(tenant,repository,execution_run),
    FOREIGN KEY(tenant,repository,check_run) REFERENCES th_native_check_runs(tenant,repository,run),
    FOREIGN KEY(tenant,repository,candidate) REFERENCES th_native_candidates(tenant,repository,candidate),
    FOREIGN KEY(tenant,repository,program) REFERENCES th_check_programs(tenant,repository,digest),
    FOREIGN KEY(tenant,repository,workspace) REFERENCES th_workspaces(tenant,repository,workspace),
    FOREIGN KEY(tenant,token) REFERENCES th_access_tokens(tenant,token),
    FOREIGN KEY(tenant,repository,receipt) REFERENCES th_native_validations(tenant,repository,receipt)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK((state='collected')=(receipt IS NOT NULL))
);
CREATE FUNCTION th_check_execution_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (to_jsonb(NEW)-ARRAY['state','receipt']) IS DISTINCT FROM
       (to_jsonb(OLD)-ARRAY['state','receipt']) OR
       (OLD.state='collected' AND NEW IS DISTINCT FROM OLD) OR
       (OLD.state='requested' AND NEW.state='preparing') THEN
        RAISE EXCEPTION 'check execution identity is immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_check_execution_identity_guard BEFORE UPDATE ON th_check_executions
    FOR EACH ROW EXECUTE FUNCTION th_check_execution_identity();
CREATE TRIGGER th_check_execution_no_delete BEFORE DELETE ON th_check_executions
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_check_execution_evidence (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    receipt uuid NOT NULL,
    check_run uuid NOT NULL,
    digest bytea NOT NULL CHECK(octet_length(digest)=32),
    definition bytea NOT NULL CHECK(octet_length(definition) BETWEEN 1 AND 65536),
    PRIMARY KEY(tenant,repository,receipt),
    UNIQUE(tenant,repository,check_run),
    FOREIGN KEY(tenant,repository,receipt) REFERENCES th_native_validations(tenant,repository,receipt)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(tenant,repository,check_run) REFERENCES th_check_executions(tenant,repository,check_run)
);
CREATE TRIGGER th_check_execution_evidence_immutable BEFORE UPDATE OR DELETE ON th_check_execution_evidence
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
