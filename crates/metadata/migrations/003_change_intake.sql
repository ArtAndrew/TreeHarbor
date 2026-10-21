CREATE TABLE th_bases (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    revision text NOT NULL,
    snapshot bytea NOT NULL CHECK (octet_length(snapshot)=32),
    pin_holder text NOT NULL,
    PRIMARY KEY (tenant,repository,revision),
    FOREIGN KEY (tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder)
);

CREATE TABLE th_workspaces (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    workspace uuid NOT NULL,
    base_revision text NOT NULL,
    principal text NOT NULL,
    profile bytea NOT NULL CHECK (octet_length(profile)=32),
    format text NOT NULL CHECK (format IN ('native','git-compatible')),
    state text NOT NULL CHECK (state IN ('requested','ready','in_use','draining','deleted')),
    generation bigint NOT NULL DEFAULT 1 CHECK (generation>0),
    epoch bigint NOT NULL CHECK (epoch>0),
    pin_holder text NOT NULL,
    node text,
    runtime text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,repository,workspace),
    FOREIGN KEY (tenant,repository,base_revision) REFERENCES th_bases(tenant,repository,revision),
    FOREIGN KEY (tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder)
);

CREATE TABLE th_agent_runs (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    run uuid NOT NULL,
    workspace uuid NOT NULL,
    workspace_generation bigint NOT NULL CHECK (workspace_generation>0),
    epoch bigint NOT NULL CHECK (epoch>0),
    principal text NOT NULL,
    agent text NOT NULL,
    task text NOT NULL,
    runtime text NOT NULL,
    state text NOT NULL CHECK (state IN ('running','stopping','succeeded','failed','cancelled','lost')),
    started_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,repository,run),
    FOREIGN KEY (tenant,repository,workspace) REFERENCES th_workspaces(tenant,repository,workspace)
);
CREATE UNIQUE INDEX th_one_active_run ON th_agent_runs(tenant,repository,workspace)
    WHERE state IN ('running','stopping');

CREATE TABLE th_changesets (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    changeset uuid NOT NULL,
    principal text NOT NULL,
    latest_version bigint NOT NULL CHECK (latest_version>0),
    PRIMARY KEY (tenant,repository,changeset),
    FOREIGN KEY (tenant,repository) REFERENCES th_repositories(tenant,repository)
);

CREATE TABLE th_change_versions (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    changeset uuid NOT NULL,
    version bigint NOT NULL CHECK (version>0),
    run uuid NOT NULL,
    base_revision text NOT NULL,
    base_snapshot bytea NOT NULL CHECK (octet_length(base_snapshot)=32),
    operations bytea NOT NULL CHECK (octet_length(operations)=32),
    result_snapshot bytea NOT NULL CHECK (octet_length(result_snapshot)=32),
    base_pin text NOT NULL,
    operations_pin text NOT NULL,
    result_pin text NOT NULL,
    operation_key text NOT NULL,
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,repository,changeset,version),
    FOREIGN KEY (tenant,repository,changeset) REFERENCES th_changesets(tenant,repository,changeset),
    FOREIGN KEY (tenant,repository,run) REFERENCES th_agent_runs(tenant,repository,run),
    FOREIGN KEY (tenant,repository,base_revision) REFERENCES th_bases(tenant,repository,revision),
    FOREIGN KEY (tenant,repository,base_pin) REFERENCES th_object_pins(tenant,repository,holder),
    FOREIGN KEY (tenant,repository,operations_pin) REFERENCES th_object_pins(tenant,repository,holder),
    FOREIGN KEY (tenant,repository,result_pin) REFERENCES th_object_pins(tenant,repository,holder),
    FOREIGN KEY (tenant,repository,operation_key) REFERENCES th_operations(tenant,repository,key)
        DEFERRABLE INITIALLY DEFERRED
);

-- Updates are always new versions. Retention deletion requires a separate
-- lifecycle transaction; no such deletion endpoint is implemented yet.
CREATE FUNCTION th_reject_immutable_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'TreeHarbor immutable record cannot be updated' USING ERRCODE='23514';
END;
$$;
CREATE TRIGGER th_change_version_immutable BEFORE UPDATE ON th_change_versions
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TRIGGER th_base_binding_immutable BEFORE UPDATE ON th_bases
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
