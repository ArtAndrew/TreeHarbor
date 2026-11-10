CREATE TABLE th_remote_tasks (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    task uuid NOT NULL,
    owner text NOT NULL,
    authority text NOT NULL CHECK (authority IN ('connected','standalone')),
    epoch bigint NOT NULL CHECK (epoch>0),
    base_revision text NOT NULL,
    profile bytea NOT NULL CHECK (octet_length(profile)=32),
    agent text NOT NULL CHECK (length(agent) BETWEEN 1 AND 128),
    label text NOT NULL CHECK (length(label) BETWEEN 1 AND 256),
    workers text[] NOT NULL CHECK (cardinality(workers) BETWEEN 1 AND 16),
    state text NOT NULL DEFAULT 'queued',
    generation bigint NOT NULL DEFAULT 0 CHECK (generation BETWEEN 0 AND 16),
    lease_until timestamptz,
    result_changeset uuid,
    result_version bigint,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,repository,task),
    FOREIGN KEY (tenant,owner) REFERENCES th_principals(tenant,principal),
    FOREIGN KEY (tenant,repository,base_revision) REFERENCES th_bases(tenant,repository,revision),
    FOREIGN KEY (tenant,repository,profile) REFERENCES th_source_profiles(tenant,repository,digest),
    FOREIGN KEY (tenant,repository,result_changeset,result_version)
        REFERENCES th_change_versions(tenant,repository,changeset,version),
    CHECK ((state='queued' AND generation=0 AND lease_until IS NULL
                AND result_changeset IS NULL AND result_version IS NULL)
        OR (state='leased' AND generation>0 AND lease_until IS NOT NULL
                AND result_changeset IS NULL AND result_version IS NULL)
        OR (state='accepted' AND generation>0 AND lease_until IS NULL
                AND result_changeset IS NOT NULL AND result_version IS NOT NULL AND result_version=1)
        OR (state='cancelled' AND lease_until IS NULL
                AND result_changeset IS NULL AND result_version IS NULL))
);
CREATE INDEX th_remote_tasks_dispatch ON th_remote_tasks(tenant,repository,epoch,created_at,task)
    WHERE state IN ('queued','leased');

CREATE TABLE th_remote_task_attempts (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    task uuid NOT NULL,
    generation bigint NOT NULL CHECK (generation BETWEEN 1 AND 16),
    worker text NOT NULL,
    workspace uuid NOT NULL,
    run uuid NOT NULL,
    PRIMARY KEY (tenant,repository,task,generation),
    UNIQUE (tenant,repository,task,worker),
    UNIQUE (tenant,repository,workspace),
    UNIQUE (tenant,repository,run),
    FOREIGN KEY (tenant,repository,task) REFERENCES th_remote_tasks(tenant,repository,task),
    FOREIGN KEY (tenant,worker) REFERENCES th_principals(tenant,principal),
    FOREIGN KEY (tenant,repository,workspace) REFERENCES th_workspaces(tenant,repository,workspace),
    FOREIGN KEY (tenant,repository,run) REFERENCES th_agent_runs(tenant,repository,run)
);
CREATE TRIGGER th_remote_attempt_immutable BEFORE UPDATE ON th_remote_task_attempts
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
