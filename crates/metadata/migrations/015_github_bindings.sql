ALTER TABLE th_repository_access DROP CONSTRAINT th_repository_access_permissions_check;
ALTER TABLE th_repository_access ADD CHECK(permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','review','policy.write','integration.control']::text[]);
ALTER TABLE th_access_tokens DROP CONSTRAINT th_access_tokens_permissions_check;
ALTER TABLE th_access_tokens ADD CHECK(cardinality(permissions)>0 AND permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','review','policy.write','integration.control']::text[]);

CREATE TABLE th_github_bindings (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    binding uuid NOT NULL UNIQUE,
    epoch bigint NOT NULL CHECK(epoch>0),
    bridge text NOT NULL,
    specification jsonb NOT NULL CHECK(jsonb_typeof(specification)='object'),
    state text NOT NULL CHECK(state IN ('active','revoked')),
    principal text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    revoked_at timestamptz,
    PRIMARY KEY(tenant,repository,binding),
    FOREIGN KEY(tenant,repository) REFERENCES th_repositories(tenant,repository),
    FOREIGN KEY(tenant,bridge) REFERENCES th_principals(tenant,principal),
    CHECK((state='revoked')=(revoked_at IS NOT NULL))
);
CREATE UNIQUE INDEX th_github_one_active_binding ON th_github_bindings(tenant,repository) WHERE state='active';
CREATE FUNCTION th_github_binding_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.tenant,NEW.repository,NEW.binding,NEW.epoch,NEW.bridge,NEW.specification,NEW.principal,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.binding,OLD.epoch,OLD.bridge,OLD.specification,OLD.principal,OLD.created_at)
       OR (OLD.state='revoked' AND NEW IS DISTINCT FROM OLD)
       OR NOT (NEW.state=OLD.state OR (OLD.state='active' AND NEW.state='revoked'))
    THEN RAISE EXCEPTION 'GitHub binding identity cannot be replaced' USING ERRCODE='23514'; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_binding_update BEFORE UPDATE ON th_github_bindings FOR EACH ROW EXECUTE FUNCTION th_github_binding_immutable();
CREATE TRIGGER th_github_binding_delete BEFORE DELETE ON th_github_bindings FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

CREATE TABLE th_github_probes (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    binding uuid NOT NULL,
    probe uuid NOT NULL UNIQUE,
    epoch bigint NOT NULL CHECK(epoch>0),
    bridge text NOT NULL,
    state text NOT NULL CHECK(state IN ('pending','observed')),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL,
    observation jsonb CHECK(jsonb_typeof(observation)='object'),
    completed_at timestamptz,
    PRIMARY KEY(tenant,repository,probe),
    FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
    CHECK(expires_at>created_at),
    CHECK((state='observed')=(observation IS NOT NULL)),
    CHECK((state='observed')=(completed_at IS NOT NULL))
);
CREATE INDEX th_github_pending_probes ON th_github_probes(tenant,repository,binding,expires_at) WHERE state='pending';
CREATE FUNCTION th_github_probe_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.tenant,NEW.repository,NEW.binding,NEW.probe,NEW.epoch,NEW.bridge,NEW.created_at,NEW.expires_at)
       IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.binding,OLD.probe,OLD.epoch,OLD.bridge,OLD.created_at,OLD.expires_at)
       OR (OLD.state='observed' AND NEW IS DISTINCT FROM OLD)
       OR NOT (NEW.state=OLD.state OR (OLD.state='pending' AND NEW.state='observed'))
    THEN RAISE EXCEPTION 'GitHub probe context or observation cannot be replaced' USING ERRCODE='23514'; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_probe_update BEFORE UPDATE ON th_github_probes FOR EACH ROW EXECUTE FUNCTION th_github_probe_immutable();
CREATE TRIGGER th_github_probe_delete BEFORE DELETE ON th_github_probes FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
