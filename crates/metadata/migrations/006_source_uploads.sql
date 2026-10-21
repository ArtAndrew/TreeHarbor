CREATE TABLE th_storage_backends (
    backend text PRIMARY KEY CHECK (backend ~ '^[a-z0-9-]{1,64}$'),
    identity uuid NOT NULL UNIQUE
);
CREATE TABLE th_source_uploads (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    upload uuid NOT NULL,
    principal text NOT NULL,
    authority text NOT NULL CHECK (authority IN ('connected','standalone')),
    epoch bigint NOT NULL CHECK (epoch>0),
    backend text NOT NULL,
    state text NOT NULL DEFAULT 'active' CHECK (state IN ('active','completed','aborted')),
    expires_at timestamptz NOT NULL,
    max_objects bigint NOT NULL CHECK (max_objects BETWEEN 1 AND 1000000),
    max_bytes bigint NOT NULL CHECK (max_bytes BETWEEN 22 AND 68719476736),
    reserved_objects bigint NOT NULL DEFAULT 0 CHECK (reserved_objects BETWEEN 0 AND max_objects),
    reserved_bytes bigint NOT NULL DEFAULT 0 CHECK (reserved_bytes BETWEEN 0 AND max_bytes),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,repository,upload),
    FOREIGN KEY (tenant,repository) REFERENCES th_repositories(tenant,repository)
);
CREATE INDEX th_source_uploads_active ON th_source_uploads(tenant,repository,expires_at) WHERE state='active';
CREATE TABLE th_source_upload_items (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    upload uuid NOT NULL,
    digest bytea NOT NULL CHECK (octet_length(digest)=32),
    generation bigint NOT NULL CHECK (generation>0),
    hold_token uuid NOT NULL,
    kind smallint NOT NULL CHECK (kind BETWEEN 1 AND 5),
    length bigint NOT NULL CHECK (length BETWEEN 22 AND 67108886),
    snapshot bytea,
    complete boolean NOT NULL DEFAULT false,
    PRIMARY KEY (tenant,repository,upload,digest),
    FOREIGN KEY (tenant,repository,upload) REFERENCES th_source_uploads(tenant,repository,upload),
    CHECK ((kind=5 AND snapshot IS NOT NULL AND octet_length(snapshot)=32) OR (kind<>5 AND snapshot IS NULL))
);
-- Deliberately no FK to a physical catalog generation or expiring hold. Closed
-- session receipts must not prevent later collection and reuse of that slot.
