CREATE TABLE th_schema_version (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    version integer NOT NULL,
    migration_digest bytea NOT NULL CHECK (octet_length(migration_digest) = 32)
);

CREATE TABLE th_repositories (
    tenant text NOT NULL CHECK (tenant ~ '^[a-z0-9-]{1,64}$'),
    repository uuid NOT NULL,
    authority text NOT NULL CHECK (authority IN ('connected', 'standalone')),
    state text NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'frozen', 'retired')),
    epoch bigint NOT NULL DEFAULT 1 CHECK (epoch > 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant, repository)
);

CREATE TABLE th_operations (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    key text NOT NULL CHECK (length(key) BETWEEN 1 AND 128),
    request_digest bytea NOT NULL CHECK (octet_length(request_digest) = 32),
    principal text NOT NULL CHECK (length(principal) BETWEEN 1 AND 128),
    kind text NOT NULL CHECK (length(kind) BETWEEN 1 AND 64),
    result jsonb NOT NULL,
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant, repository, key),
    FOREIGN KEY (tenant, repository) REFERENCES th_repositories(tenant, repository)
);

CREATE TABLE th_outbox (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant text NOT NULL,
    repository uuid NOT NULL,
    operation_key text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    kind text NOT NULL CHECK (length(kind) BETWEEN 1 AND 64),
    payload jsonb NOT NULL,
    state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'running', 'done')),
    generation bigint NOT NULL DEFAULT 0 CHECK (generation >= 0),
    worker uuid,
    lease_until timestamptz,
    available_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    UNIQUE (tenant, repository, operation_key, ordinal),
    FOREIGN KEY (tenant, repository, operation_key)
      REFERENCES th_operations(tenant, repository, key),
    CHECK ((state = 'running' AND worker IS NOT NULL AND lease_until IS NOT NULL)
        OR (state <> 'running' AND worker IS NULL AND lease_until IS NULL))
);
CREATE INDEX th_outbox_pending ON th_outbox (tenant, available_at, id) WHERE state = 'pending';
CREATE INDEX th_outbox_expired ON th_outbox (tenant, lease_until, id) WHERE state = 'running';
