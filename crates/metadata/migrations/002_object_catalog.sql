CREATE TABLE th_objects (
    tenant text NOT NULL CHECK (tenant ~ '^[a-z0-9-]{1,64}$'),
    digest bytea NOT NULL CHECK (octet_length(digest)=32),
    generation bigint NOT NULL CHECK (generation>0),
    backend text NOT NULL CHECK (backend ~ '^[a-z0-9-]{1,64}$'),
    kind smallint NOT NULL CHECK (kind BETWEEN 1 AND 5),
    length bigint NOT NULL CHECK (length BETWEEN 22 AND 67108886),
    state text NOT NULL CHECK (state IN ('uploading','live','retiring','deleted')),
    orphaned_at timestamptz,
    PRIMARY KEY (tenant,digest),
    UNIQUE (tenant,digest,generation)
);
CREATE TABLE th_object_holds (
    tenant text NOT NULL,
    token uuid NOT NULL,
    digest bytea NOT NULL,
    generation bigint NOT NULL,
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (tenant,token),
    FOREIGN KEY (tenant,digest,generation) REFERENCES th_objects(tenant,digest,generation)
);
CREATE INDEX th_object_holds_object ON th_object_holds(tenant,digest,generation,expires_at);
CREATE TABLE th_object_edges (
    tenant text NOT NULL,
    parent bytea NOT NULL,
    parent_generation bigint NOT NULL,
    child bytea NOT NULL,
    child_generation bigint NOT NULL,
    PRIMARY KEY (tenant,parent,parent_generation,child,child_generation),
    FOREIGN KEY (tenant,parent,parent_generation) REFERENCES th_objects(tenant,digest,generation),
    FOREIGN KEY (tenant,child,child_generation) REFERENCES th_objects(tenant,digest,generation),
    CHECK (parent<>child)
);
CREATE INDEX th_object_edges_inbound ON th_object_edges(tenant,child,child_generation);
CREATE TABLE th_object_pins (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    holder text NOT NULL CHECK (length(holder) BETWEEN 1 AND 128),
    digest bytea NOT NULL,
    generation bigint NOT NULL,
    PRIMARY KEY (tenant,repository,holder),
    FOREIGN KEY (tenant,repository) REFERENCES th_repositories(tenant,repository),
    FOREIGN KEY (tenant,digest,generation) REFERENCES th_objects(tenant,digest,generation)
);
CREATE INDEX th_object_pins_object ON th_object_pins(tenant,digest,generation);
-- Tombstones survive reuse of the logical object slot. Delayed deletion may
-- operate only on this old physical generation, never a digest-only location.
CREATE TABLE th_object_retirements (
    tenant text NOT NULL,
    digest bytea NOT NULL CHECK (octet_length(digest)=32),
    generation bigint NOT NULL,
    backend text NOT NULL,
    retired_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    PRIMARY KEY (tenant,digest,generation)
);
