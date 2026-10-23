-- Released placements retain their historical identity, but no longer occupy a
-- slot. Physical deletion must precede this transition; expiry is insufficient.
ALTER TABLE th_workspaces ALTER COLUMN pin_holder DROP NOT NULL;
ALTER TABLE th_workspaces ADD CONSTRAINT th_live_workspace_pin
    CHECK (state='deleted' OR pin_holder IS NOT NULL);
ALTER TABLE th_workspace_preparations DROP CONSTRAINT th_workspace_preparations_state_check;
ALTER TABLE th_workspace_preparations ADD CONSTRAINT th_workspace_preparations_state_check
    CHECK (state IN ('reserved','prepared','draining','released'));
ALTER TABLE th_workspace_preparations DROP CONSTRAINT th_workspace_preparations_storage_instance_slot_key;
CREATE UNIQUE INDEX th_preparations_occupied_slot ON th_workspace_preparations(storage_instance,slot)
    WHERE state<>'released';

CREATE OR REPLACE FUNCTION th_preparation_identity_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT (NEW.state=OLD.state OR (OLD.state='reserved' AND NEW.state IN ('prepared','draining'))
            OR (OLD.state='prepared' AND NEW.state='draining')
            OR (OLD.state='draining' AND NEW.state='released')) THEN
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

CREATE TABLE th_workspace_cleanups (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    workspace uuid NOT NULL,
    cleanup uuid NOT NULL UNIQUE,
    workspace_generation bigint NOT NULL CHECK(workspace_generation>1),
    epoch bigint NOT NULL CHECK(epoch>0),
    authority text NOT NULL CHECK(authority IN ('connected','standalone')),
    node text NOT NULL,
    storage_instance uuid NOT NULL,
    preparation uuid REFERENCES th_workspace_preparations(attempt),
    run uuid,
    state text NOT NULL CHECK(state IN ('claimed','deleting','deleted')),
    claim_generation bigint NOT NULL CHECK(claim_generation>0),
    worker uuid NOT NULL,
    lease_until timestamptz NOT NULL,
    intent_digest bytea CHECK(octet_length(intent_digest)=32),
    receipt_digest bytea CHECK(octet_length(receipt_digest)=32),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    PRIMARY KEY(tenant,repository,workspace),
    FOREIGN KEY(tenant,repository,workspace) REFERENCES th_workspaces(tenant,repository,workspace),
    FOREIGN KEY(tenant,node,storage_instance) REFERENCES th_node_storage(tenant,node,storage_instance),
    FOREIGN KEY(tenant,repository,run) REFERENCES th_managed_runs(tenant,repository,run),
    CHECK(run IS NULL OR preparation IS NOT NULL),
    CHECK((state='claimed')=(intent_digest IS NULL)),
    CHECK((state='deleted')=(receipt_digest IS NOT NULL)),
    CHECK((state='deleted')=(completed_at IS NOT NULL))
);
CREATE FUNCTION th_cleanup_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.tenant,NEW.repository,NEW.workspace,NEW.cleanup,NEW.workspace_generation,
           NEW.epoch,NEW.authority,NEW.node,NEW.storage_instance,NEW.preparation,NEW.run,NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.tenant,OLD.repository,OLD.workspace,OLD.cleanup,OLD.workspace_generation,
           OLD.epoch,OLD.authority,OLD.node,OLD.storage_instance,OLD.preparation,OLD.run,OLD.created_at)
       OR NOT (NEW.state=OLD.state OR (OLD.state='claimed' AND NEW.state='deleting')
              OR (OLD.state='deleting' AND NEW.state='deleted'))
       OR (OLD.intent_digest IS NOT NULL AND NEW.intent_digest IS DISTINCT FROM OLD.intent_digest)
       OR (OLD.state='deleted' AND NEW IS DISTINCT FROM OLD)
    THEN
        RAISE EXCEPTION 'TreeHarbor cleanup identity or receipt cannot be replaced' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_cleanup_identity BEFORE UPDATE ON th_workspace_cleanups
    FOR EACH ROW EXECUTE FUNCTION th_cleanup_immutable();
