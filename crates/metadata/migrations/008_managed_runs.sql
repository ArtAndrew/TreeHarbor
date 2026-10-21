CREATE TABLE th_runtime_profiles (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    digest bytea NOT NULL CHECK(octet_length(digest)=32),
    definition bytea NOT NULL CHECK(octet_length(definition) BETWEEN 1 AND 4096),
    PRIMARY KEY(tenant,repository,digest),
    FOREIGN KEY(tenant,repository) REFERENCES th_repositories(tenant,repository)
);
CREATE TRIGGER th_runtime_profile_immutable BEFORE UPDATE ON th_runtime_profiles
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_runtime_profile_revocations (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    digest bytea NOT NULL,
    revoked_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,digest),
    FOREIGN KEY(tenant,repository,digest) REFERENCES th_runtime_profiles(tenant,repository,digest)
);
CREATE TRIGGER th_runtime_profile_revocation_immutable BEFORE UPDATE ON th_runtime_profile_revocations
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_node_volumes (
    tenant text NOT NULL,
    node text NOT NULL,
    storage_instance uuid NOT NULL,
    slot uuid NOT NULL,
    identity_digest bytea NOT NULL CHECK(octet_length(identity_digest)=32),
    limit_bytes bigint NOT NULL CHECK(limit_bytes BETWEEN 67108864 AND 68719476736),
    PRIMARY KEY(storage_instance,slot),
    FOREIGN KEY(tenant,node,storage_instance) REFERENCES th_node_storage(tenant,node,storage_instance)
);
CREATE TRIGGER th_node_volume_immutable BEFORE UPDATE ON th_node_volumes
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

ALTER TABLE th_agent_runs DROP CONSTRAINT th_agent_runs_state_check;
ALTER TABLE th_agent_runs ADD CONSTRAINT th_agent_runs_state_check CHECK
    (state IN ('requested','launching','running','stopping','succeeded','failed','cancelled','lost'));
ALTER TABLE th_agent_runs ALTER COLUMN runtime DROP NOT NULL;
ALTER TABLE th_agent_runs ALTER COLUMN started_at DROP NOT NULL;
ALTER TABLE th_agent_runs ADD CONSTRAINT th_running_observation_required CHECK
    (state <> 'running' OR (runtime IS NOT NULL AND started_at IS NOT NULL));
DROP INDEX th_one_active_run;
CREATE UNIQUE INDEX th_one_active_run ON th_agent_runs(tenant,repository,workspace)
    WHERE state IN ('requested','launching','running','stopping');

CREATE TABLE th_managed_runs (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    run uuid NOT NULL,
    attempt uuid NOT NULL UNIQUE,
    preparation uuid NOT NULL UNIQUE REFERENCES th_workspace_preparations(attempt),
    node text NOT NULL,
    storage_instance uuid NOT NULL,
    slot uuid NOT NULL,
    volume_digest bytea NOT NULL CHECK(octet_length(volume_digest)=32),
    profile bytea NOT NULL,
    argv bytea NOT NULL CHECK(octet_length(argv) BETWEEN 1 AND 393216),
    binding bytea NOT NULL CHECK(octet_length(binding)=32),
    operation_key text NOT NULL,
    authority text NOT NULL CHECK(authority IN ('connected','standalone')),
    stop_requested boolean NOT NULL DEFAULT false,
    engine text CHECK(octet_length(engine) BETWEEN 1 AND 128),
    container text CHECK(container ~ '^[0-9a-f]{64}$'),
    exit_code bigint,
    oom_killed boolean,
    runtime_started_at text CHECK(octet_length(runtime_started_at) BETWEEN 1 AND 128),
    runtime_finished_at text CHECK(octet_length(runtime_finished_at) BETWEEN 1 AND 128),
    removed boolean NOT NULL DEFAULT false,
    finished_generation bigint CHECK(finished_generation>0),
    finished_worker uuid,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,run),
    FOREIGN KEY(tenant,repository,run) REFERENCES th_agent_runs(tenant,repository,run),
    FOREIGN KEY(tenant,repository,profile) REFERENCES th_runtime_profiles(tenant,repository,digest),
    FOREIGN KEY(storage_instance,slot) REFERENCES th_node_volumes(storage_instance,slot),
    FOREIGN KEY(tenant,repository,operation_key) REFERENCES th_operations(tenant,repository,key)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK((engine IS NULL)=(container IS NULL)),
    CHECK((NOT removed AND finished_generation IS NULL AND finished_worker IS NULL)
        OR (removed AND finished_generation IS NOT NULL AND finished_worker IS NOT NULL)),
    CHECK((exit_code IS NULL AND oom_killed IS NULL AND runtime_started_at IS NULL AND runtime_finished_at IS NULL)
        OR (exit_code IS NOT NULL AND oom_killed IS NOT NULL AND runtime_started_at IS NOT NULL AND runtime_finished_at IS NOT NULL AND container IS NOT NULL))
);
CREATE FUNCTION th_managed_run_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.tenant,NEW.repository,NEW.run,NEW.attempt,NEW.preparation,NEW.node,NEW.storage_instance,
           NEW.slot,NEW.volume_digest,NEW.profile,NEW.argv,NEW.binding,NEW.operation_key,NEW.authority,NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.tenant,OLD.repository,OLD.run,OLD.attempt,OLD.preparation,OLD.node,OLD.storage_instance,
           OLD.slot,OLD.volume_digest,OLD.profile,OLD.argv,OLD.binding,OLD.operation_key,OLD.authority,OLD.created_at)
       OR (OLD.stop_requested AND NOT NEW.stop_requested) OR (OLD.removed AND NEW IS DISTINCT FROM OLD)
       OR (OLD.container IS NOT NULL AND ROW(NEW.engine,NEW.container) IS DISTINCT FROM ROW(OLD.engine,OLD.container))
       OR (OLD.exit_code IS NOT NULL AND ROW(NEW.exit_code,NEW.oom_killed,NEW.runtime_started_at,NEW.runtime_finished_at)
            IS DISTINCT FROM ROW(OLD.exit_code,OLD.oom_killed,OLD.runtime_started_at,OLD.runtime_finished_at))
    THEN
        RAISE EXCEPTION 'TreeHarbor run identity or observation cannot be replaced' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_managed_run_identity BEFORE UPDATE ON th_managed_runs
    FOR EACH ROW EXECUTE FUNCTION th_managed_run_immutable();
CREATE FUNCTION th_managed_agent_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS(SELECT 1 FROM th_managed_runs WHERE tenant=OLD.tenant AND repository=OLD.repository AND run=OLD.run)
       AND (ROW(NEW.tenant,NEW.repository,NEW.run,NEW.workspace,NEW.workspace_generation,NEW.epoch,NEW.principal,NEW.agent,NEW.task)
            IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.run,OLD.workspace,OLD.workspace_generation,OLD.epoch,OLD.principal,OLD.agent,OLD.task)
         OR (OLD.runtime IS NOT NULL AND NEW.runtime IS DISTINCT FROM OLD.runtime)
         OR (OLD.started_at IS NOT NULL AND NEW.started_at IS DISTINCT FROM OLD.started_at)
         OR (OLD.state IN ('succeeded','failed','cancelled','lost') AND NEW IS DISTINCT FROM OLD))
    THEN
        RAISE EXCEPTION 'TreeHarbor managed agent identity or terminal result cannot be replaced' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_managed_agent_identity BEFORE UPDATE ON th_agent_runs
    FOR EACH ROW EXECUTE FUNCTION th_managed_agent_identity();
CREATE INDEX th_run_jobs_due ON th_outbox(tenant,repository,id)
    WHERE kind='run.execute' AND state IN ('pending','running');
