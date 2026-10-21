CREATE TABLE th_change_uploads (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    upload uuid NOT NULL,
    principal text NOT NULL,
    authority text NOT NULL CHECK (authority IN ('connected','standalone')),
    epoch bigint NOT NULL CHECK (epoch>0),
    run uuid NOT NULL,
    workspace_generation bigint NOT NULL CHECK (workspace_generation>0),
    backend text NOT NULL,
    state text NOT NULL DEFAULT 'active' CHECK (state IN ('active','completed','aborted')),
    expires_at timestamptz NOT NULL,
    max_objects bigint NOT NULL CHECK (max_objects BETWEEN 1 AND 1000000),
    max_bytes bigint NOT NULL CHECK (max_bytes BETWEEN 22 AND 68719476736),
    reserved_objects bigint NOT NULL DEFAULT 0 CHECK (reserved_objects BETWEEN 0 AND max_objects),
    reserved_bytes bigint NOT NULL DEFAULT 0 CHECK (reserved_bytes BETWEEN 0 AND max_bytes),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (tenant,repository,upload),
    FOREIGN KEY (tenant,repository,run) REFERENCES th_agent_runs(tenant,repository,run)
);
CREATE INDEX th_change_uploads_active ON th_change_uploads(tenant,repository,principal,expires_at) WHERE state='active';
CREATE TABLE th_change_upload_items (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    upload uuid NOT NULL,
    digest bytea NOT NULL CHECK (octet_length(digest)=32),
    generation bigint NOT NULL CHECK (generation>0),
    hold_token uuid NOT NULL,
    length bigint NOT NULL CHECK (length BETWEEN 22 AND 67108886),
    complete boolean NOT NULL DEFAULT false,
    PRIMARY KEY (tenant,repository,upload,digest),
    FOREIGN KEY (tenant,repository,upload) REFERENCES th_change_uploads(tenant,repository,upload)
);
-- Historical sessions cannot prevent collection after their bounded holds expire.
CREATE TABLE th_change_derived_items (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    upload uuid NOT NULL,
    digest bytea NOT NULL CHECK (octet_length(digest)=32),
    generation bigint NOT NULL CHECK (generation>0),
    hold_token uuid NOT NULL,
    kind smallint NOT NULL CHECK (kind BETWEEN 2 AND 4),
    length bigint NOT NULL CHECK (length BETWEEN 22 AND 67108886),
    PRIMARY KEY (tenant,repository,upload,digest),
    FOREIGN KEY (tenant,repository,upload) REFERENCES th_change_uploads(tenant,repository,upload)
);
-- Only complete immutable blob frames enter this table; generated trees are built
-- by the service and published through the existing reserved-object catalog.
CREATE FUNCTION th_change_upload_identity_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (NEW.tenant,NEW.repository,NEW.upload,NEW.principal,NEW.authority,NEW.epoch,
        NEW.run,NEW.workspace_generation,NEW.backend,NEW.expires_at,
        NEW.max_objects,NEW.max_bytes,NEW.created_at)
        IS DISTINCT FROM
       (OLD.tenant,OLD.repository,OLD.upload,OLD.principal,OLD.authority,OLD.epoch,
        OLD.run,OLD.workspace_generation,OLD.backend,OLD.expires_at,
        OLD.max_objects,OLD.max_bytes,OLD.created_at)
       OR (OLD.state<>'active' AND NEW IS DISTINCT FROM OLD)
       OR NEW.reserved_objects<OLD.reserved_objects OR NEW.reserved_bytes<OLD.reserved_bytes
    THEN RAISE EXCEPTION 'immutable change upload'; END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER th_change_upload_identity BEFORE UPDATE ON th_change_uploads
    FOR EACH ROW EXECUTE FUNCTION th_change_upload_identity_immutable();
