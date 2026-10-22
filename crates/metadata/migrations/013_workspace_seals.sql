-- Owner-selected stopped-file capture. A database row is not runtime removal
-- proof: only the assigned node may publish after inspecting its exact journal.
CREATE TABLE th_workspace_seals (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    seal uuid NOT NULL,
    principal text NOT NULL,
    run uuid NOT NULL,
    workspace_generation bigint NOT NULL CHECK(workspace_generation>0),
    authority text NOT NULL CHECK(authority IN ('connected','standalone')),
    epoch bigint NOT NULL CHECK(epoch>0),
    selection bytea NOT NULL CHECK(octet_length(selection) BETWEEN 1 AND 65536),
    base_pin text NOT NULL,
    max_objects bigint NOT NULL CHECK(max_objects BETWEEN 1 AND 1000000),
    max_bytes bigint NOT NULL CHECK(max_bytes BETWEEN 22 AND 68719476736),
    expires_at timestamptz NOT NULL,
    state text NOT NULL CHECK(state IN ('waiting','capturing','ready','failed','cancelled')),
    claim_generation bigint NOT NULL DEFAULT 0 CHECK(claim_generation>=0),
    worker uuid,
    lease_until timestamptz,
    manifest_digest bytea CHECK(octet_length(manifest_digest)=32),
    operations bytea CHECK(octet_length(operations)=32),
    operations_pin text,
    failure text CHECK(failure IN ('capture_conflict','unsafe_files','resource_limit','physical_identity')),
    completed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,seal),
    FOREIGN KEY(tenant,repository,run) REFERENCES th_managed_runs(tenant,repository,run),
    FOREIGN KEY(tenant,repository,base_pin) REFERENCES th_object_pins(tenant,repository,holder),
    FOREIGN KEY(tenant,repository,operations_pin) REFERENCES th_object_pins(tenant,repository,holder),
    CHECK((state='capturing' AND worker IS NOT NULL AND lease_until IS NOT NULL AND claim_generation>0)
        OR (state<>'capturing' AND worker IS NULL AND lease_until IS NULL)),
    CHECK(lease_until IS NULL OR lease_until<=expires_at),
    CHECK((state='ready' AND manifest_digest IS NOT NULL AND operations IS NOT NULL AND operations_pin IS NOT NULL)
        OR (state<>'ready' AND manifest_digest IS NULL AND operations IS NULL AND operations_pin IS NULL)),
    CHECK((state='failed')=(failure IS NOT NULL)),
    CHECK((state IN ('ready','failed','cancelled'))=(completed_at IS NOT NULL))
);
CREATE UNIQUE INDEX th_one_pending_seal ON th_workspace_seals(tenant,repository,run)
    WHERE state IN ('waiting','capturing');
CREATE FUNCTION th_workspace_seal_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.tenant,NEW.repository,NEW.seal,NEW.principal,NEW.run,NEW.workspace_generation,
           NEW.authority,NEW.epoch,NEW.selection,NEW.base_pin,NEW.max_objects,NEW.max_bytes,NEW.expires_at,NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.tenant,OLD.repository,OLD.seal,OLD.principal,OLD.run,OLD.workspace_generation,
           OLD.authority,OLD.epoch,OLD.selection,OLD.base_pin,OLD.max_objects,OLD.max_bytes,OLD.expires_at,OLD.created_at)
       OR NEW.claim_generation<OLD.claim_generation
       OR (OLD.state IN ('ready','failed','cancelled') AND NEW IS DISTINCT FROM OLD)
       OR (OLD.state='capturing' AND NEW.state='waiting')
    THEN
        RAISE EXCEPTION 'TreeHarbor seal input or terminal result is immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_workspace_seal_identity BEFORE UPDATE ON th_workspace_seals
    FOR EACH ROW EXECUTE FUNCTION th_workspace_seal_immutable();
