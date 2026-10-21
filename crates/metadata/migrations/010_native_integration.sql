ALTER TABLE th_principals DROP CONSTRAINT th_principals_kind_check;
ALTER TABLE th_principals ADD CHECK(kind IN ('human','agent','node','validator','bridge','integrator'));
ALTER TABLE th_repository_access DROP CONSTRAINT th_repository_access_permissions_check;
ALTER TABLE th_repository_access ADD CHECK(permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.publish','review','policy.write','integration.control']::text[]);
ALTER TABLE th_access_tokens DROP CONSTRAINT th_access_tokens_permissions_check;
ALTER TABLE th_access_tokens ADD CHECK(cardinality(permissions)>0 AND permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.publish','review','policy.write','integration.control']::text[]);

CREATE TABLE th_native_policies (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    digest bytea NOT NULL CHECK(octet_length(digest)=32),
    definition bytea NOT NULL CHECK(octet_length(definition) BETWEEN 1 AND 65536),
    principal text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,digest),
    FOREIGN KEY(tenant,repository) REFERENCES th_repositories(tenant,repository)
);
CREATE TRIGGER th_native_policy_immutable BEFORE UPDATE OR DELETE ON th_native_policies
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

CREATE TABLE th_native_refs (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    name text NOT NULL CHECK(length(name) BETWEEN 1 AND 128),
    head bytea NOT NULL CHECK(octet_length(head)=32),
    generation bigint NOT NULL CHECK(generation>0),
    epoch bigint NOT NULL CHECK(epoch>0),
    policy bytea NOT NULL,
    pin_holder text NOT NULL,
    PRIMARY KEY(tenant,repository,name),
    FOREIGN KEY(tenant,repository,policy) REFERENCES th_native_policies(tenant,repository,digest),
    FOREIGN KEY(tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder)
);

CREATE TABLE th_native_queue (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    entry uuid NOT NULL,
    position bigint GENERATED ALWAYS AS IDENTITY,
    target text NOT NULL,
    epoch bigint NOT NULL CHECK(epoch>0),
    principal text NOT NULL,
    changeset uuid NOT NULL,
    version bigint NOT NULL,
    state text NOT NULL CHECK(state IN ('queued','preparing','checking','landed','conflicted','failed','stale','cancelled')),
    candidate uuid,
    reason text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,entry),
    FOREIGN KEY(tenant,repository,target) REFERENCES th_native_refs(tenant,repository,name),
    FOREIGN KEY(tenant,repository,changeset,version) REFERENCES th_change_versions(tenant,repository,changeset,version)
);
CREATE UNIQUE INDEX th_native_one_active ON th_native_queue(tenant,repository,target)
    WHERE state IN ('preparing','checking');
CREATE INDEX th_native_queue_order ON th_native_queue(tenant,repository,target,position)
    WHERE state IN ('queued','preparing','checking');
CREATE UNIQUE INDEX th_native_one_proposal ON th_native_queue(tenant,repository,target,changeset,version)
    WHERE state IN ('queued','preparing','checking','landed');

CREATE TABLE th_native_candidates (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    candidate uuid NOT NULL,
    entry uuid NOT NULL,
    expected_head bytea NOT NULL CHECK(octet_length(expected_head)=32),
    expected_snapshot bytea NOT NULL CHECK(octet_length(expected_snapshot)=32),
    expected_generation bigint NOT NULL CHECK(expected_generation>0),
    epoch bigint NOT NULL CHECK(epoch>0),
    policy bytea NOT NULL,
    merge_profile text NOT NULL,
    timestamp_seconds bigint NOT NULL,
    state text NOT NULL CHECK(state IN ('preparing','checking','landed','conflicted','failed','stale','cancelled')),
    revision bytea CHECK(octet_length(revision)=32),
    snapshot bytea CHECK(octet_length(snapshot)=32),
    pin_holder text,
    failure jsonb CHECK(failure IS NULL OR octet_length(failure::text)<=65536),
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,candidate),
    UNIQUE(tenant,repository,entry),
    FOREIGN KEY(tenant,repository,entry) REFERENCES th_native_queue(tenant,repository,entry),
    FOREIGN KEY(tenant,repository,policy) REFERENCES th_native_policies(tenant,repository,digest),
    FOREIGN KEY(tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder),
    CHECK((revision IS NULL AND snapshot IS NULL AND pin_holder IS NULL) OR
          (revision IS NOT NULL AND snapshot IS NOT NULL AND pin_holder IS NOT NULL)),
    CHECK(state NOT IN ('checking','landed') OR revision IS NOT NULL)
);
ALTER TABLE th_native_queue ADD FOREIGN KEY(tenant,repository,candidate)
    REFERENCES th_native_candidates(tenant,repository,candidate);

CREATE TABLE th_candidate_objects (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    candidate uuid NOT NULL,
    digest bytea NOT NULL CHECK(octet_length(digest)=32),
    generation bigint NOT NULL CHECK(generation>0),
    hold_token uuid NOT NULL,
    length bigint NOT NULL CHECK(length>0),
    backend text NOT NULL,
    PRIMARY KEY(tenant,repository,candidate,digest),
    FOREIGN KEY(tenant,repository,candidate) REFERENCES th_native_candidates(tenant,repository,candidate)
);

CREATE TABLE th_native_check_runs (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    candidate uuid NOT NULL,
    name text NOT NULL,
    run uuid NOT NULL,
    validator text NOT NULL,
    context bytea NOT NULL CHECK(octet_length(context)=32),
    revoked boolean NOT NULL DEFAULT false,
    PRIMARY KEY(tenant,repository,run),
    UNIQUE(tenant,repository,candidate,name,run),
    FOREIGN KEY(tenant,repository,candidate) REFERENCES th_native_candidates(tenant,repository,candidate),
    FOREIGN KEY(tenant,validator) REFERENCES th_principals(tenant,principal)
);
CREATE TABLE th_native_current_checks (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    candidate uuid NOT NULL,
    name text NOT NULL,
    run uuid NOT NULL,
    PRIMARY KEY(tenant,repository,candidate,name),
    FOREIGN KEY(tenant,repository,candidate,name,run) REFERENCES th_native_check_runs(tenant,repository,candidate,name,run)
);
CREATE TABLE th_native_validations (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    receipt uuid NOT NULL,
    run uuid NOT NULL,
    principal text NOT NULL,
    token uuid NOT NULL,
    context bytea NOT NULL CHECK(octet_length(context)=32),
    outcome text NOT NULL CHECK(outcome IN ('passed','failed','error','cancelled')),
    evidence bytea NOT NULL CHECK(octet_length(evidence)=32),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,receipt),
    FOREIGN KEY(tenant,repository,run) REFERENCES th_native_check_runs(tenant,repository,run),
    FOREIGN KEY(tenant,token) REFERENCES th_access_tokens(tenant,token)
);
CREATE TRIGGER th_native_validation_immutable BEFORE UPDATE OR DELETE ON th_native_validations
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE INDEX th_native_validation_run ON th_native_validations(tenant,repository,run);

CREATE TABLE th_native_reviews (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    review uuid NOT NULL,
    candidate uuid NOT NULL,
    principal text NOT NULL,
    token uuid NOT NULL,
    decision text NOT NULL CHECK(decision IN ('approve','reject')),
    revoked boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,review),
    FOREIGN KEY(tenant,repository,candidate) REFERENCES th_native_candidates(tenant,repository,candidate),
    FOREIGN KEY(tenant,token) REFERENCES th_access_tokens(tenant,token)
);

CREATE TABLE th_native_ref_updates (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    target text NOT NULL,
    generation bigint NOT NULL CHECK(generation>0),
    old_head bytea CHECK(octet_length(old_head)=32),
    new_head bytea NOT NULL CHECK(octet_length(new_head)=32),
    policy bytea NOT NULL CHECK(octet_length(policy)=32),
    epoch bigint NOT NULL CHECK(epoch>0),
    candidate uuid,
    principal text NOT NULL,
    operation_key text NOT NULL,
    kind text NOT NULL CHECK(kind IN ('create','policy','land')),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,target,generation),
    UNIQUE(tenant,repository,candidate),
    FOREIGN KEY(tenant,repository,target) REFERENCES th_native_refs(tenant,repository,name),
    FOREIGN KEY(tenant,repository,candidate) REFERENCES th_native_candidates(tenant,repository,candidate),
    FOREIGN KEY(tenant,repository,operation_key) REFERENCES th_operations(tenant,repository,key) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER th_native_ref_update_immutable BEFORE UPDATE OR DELETE ON th_native_ref_updates
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

CREATE FUNCTION th_native_candidate_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (to_jsonb(NEW)-ARRAY['state','revision','snapshot','pin_holder','failure']) IS DISTINCT FROM
       (to_jsonb(OLD)-ARRAY['state','revision','snapshot','pin_holder','failure']) OR
       (OLD.failure IS NOT NULL AND NEW.failure IS DISTINCT FROM OLD.failure) OR
       (OLD.revision IS NOT NULL AND (NEW.revision,NEW.snapshot,NEW.pin_holder) IS DISTINCT FROM
                                   (OLD.revision,OLD.snapshot,OLD.pin_holder)) THEN
        RAISE EXCEPTION 'native candidate identity is immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_native_candidate_identity_guard BEFORE UPDATE ON th_native_candidates
    FOR EACH ROW EXECUTE FUNCTION th_native_candidate_identity();

CREATE FUNCTION th_native_revocation_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (to_jsonb(NEW)-'revoked') IS DISTINCT FROM (to_jsonb(OLD)-'revoked') OR
       (OLD.revoked AND NOT NEW.revoked) THEN
        RAISE EXCEPTION 'native evidence identity and revocation are immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_native_check_identity_guard BEFORE UPDATE ON th_native_check_runs
    FOR EACH ROW EXECUTE FUNCTION th_native_revocation_only();
CREATE TRIGGER th_native_review_identity_guard BEFORE UPDATE ON th_native_reviews
    FOR EACH ROW EXECUTE FUNCTION th_native_revocation_only();

CREATE TRIGGER th_native_candidate_no_delete BEFORE DELETE ON th_native_candidates
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TRIGGER th_native_check_no_delete BEFORE DELETE ON th_native_check_runs
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TRIGGER th_native_review_no_delete BEFORE DELETE ON th_native_reviews
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
