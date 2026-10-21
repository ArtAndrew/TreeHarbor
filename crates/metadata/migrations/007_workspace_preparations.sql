-- Operator-provisioned identity of one node's persistent workspace storage.
-- A new UUID does not authorize adopting or deleting an old node's data.
CREATE TABLE th_node_storage (
    tenant text NOT NULL,
    node text NOT NULL,
    storage_instance uuid NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,node),
    UNIQUE (tenant,node,storage_instance),
    FOREIGN KEY (tenant,node) REFERENCES th_principals(tenant,principal)
);
CREATE TRIGGER th_node_storage_immutable BEFORE UPDATE ON th_node_storage
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

CREATE TABLE th_workspace_preparations (
    attempt uuid PRIMARY KEY,
    tenant text NOT NULL,
    repository uuid NOT NULL,
    workspace uuid NOT NULL,
    generation bigint NOT NULL CHECK (generation>0),
    epoch bigint NOT NULL CHECK (epoch>0),
    node text NOT NULL,
    storage_instance uuid NOT NULL,
    slot uuid NOT NULL,
    job_id bigint NOT NULL REFERENCES th_outbox(id),
    base_revision text NOT NULL,
    snapshot bytea NOT NULL CHECK (octet_length(snapshot)=32),
    profile bytea NOT NULL CHECK (octet_length(profile)=32),
    state text NOT NULL CHECK (state IN ('reserved','prepared','draining')),
    manifest_digest bytea CHECK (octet_length(manifest_digest)=32),
    completed_worker uuid,
    completed_job_generation bigint,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    prepared_at timestamptz,
    UNIQUE (tenant,repository,workspace,generation),
    -- Retained even while draining. No reuse without a future cleanup protocol.
    UNIQUE (storage_instance,slot),
    FOREIGN KEY (tenant,repository,workspace) REFERENCES th_workspaces(tenant,repository,workspace),
    FOREIGN KEY (tenant,node,storage_instance) REFERENCES th_node_storage(tenant,node,storage_instance),
    FOREIGN KEY (tenant,repository,base_revision) REFERENCES th_bases(tenant,repository,revision),
    FOREIGN KEY (tenant,repository,profile) REFERENCES th_source_profiles(tenant,repository,digest),
    CHECK ((manifest_digest IS NULL AND completed_worker IS NULL AND completed_job_generation IS NULL AND prepared_at IS NULL)
        OR (manifest_digest IS NOT NULL AND completed_worker IS NOT NULL AND completed_job_generation IS NOT NULL AND completed_job_generation>0 AND prepared_at IS NOT NULL)),
    CHECK (state<>'reserved' OR manifest_digest IS NULL),
    CHECK (state<>'prepared' OR manifest_digest IS NOT NULL)
);
CREATE INDEX th_preparations_node ON th_workspace_preparations(tenant,node,state);
CREATE INDEX th_preparation_jobs_due ON th_outbox(tenant,repository,id)
    WHERE kind='workspace.prepare' AND state IN ('pending','running');

-- Identity/source bindings never change; only observed lifecycle fields may.
CREATE FUNCTION th_preparation_identity_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT (NEW.state=OLD.state OR (OLD.state='reserved' AND NEW.state IN ('prepared','draining'))
            OR (OLD.state='prepared' AND NEW.state='draining')) THEN
        RAISE EXCEPTION 'TreeHarbor preparation state cannot move backwards' USING ERRCODE='23514';
    END IF;
    IF ROW(NEW.attempt,NEW.tenant,NEW.repository,NEW.workspace,NEW.generation,NEW.epoch,NEW.node,
           NEW.storage_instance,NEW.slot,NEW.job_id,NEW.base_revision,NEW.snapshot,NEW.profile,NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.attempt,OLD.tenant,OLD.repository,OLD.workspace,OLD.generation,OLD.epoch,OLD.node,
           OLD.storage_instance,OLD.slot,OLD.job_id,OLD.base_revision,OLD.snapshot,OLD.profile,OLD.created_at)
    THEN
        RAISE EXCEPTION 'TreeHarbor preparation identity is immutable' USING ERRCODE='23514';
    END IF;
    IF OLD.manifest_digest IS NOT NULL AND
       ROW(NEW.manifest_digest,NEW.completed_worker,NEW.completed_job_generation,NEW.prepared_at)
       IS DISTINCT FROM
       ROW(OLD.manifest_digest,OLD.completed_worker,OLD.completed_job_generation,OLD.prepared_at)
    THEN
        RAISE EXCEPTION 'TreeHarbor preparation observation is immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_preparation_identity BEFORE UPDATE ON th_workspace_preparations
    FOR EACH ROW EXECUTE FUNCTION th_preparation_identity_immutable();
