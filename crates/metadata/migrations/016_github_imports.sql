CREATE TABLE th_github_imports (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    import uuid NOT NULL UNIQUE,
    binding uuid NOT NULL,
    epoch bigint NOT NULL CHECK(epoch>0),
    principal text NOT NULL,
    bridge text NOT NULL,
    git_revision text NOT NULL CHECK(git_revision ~ '^git:(sha1:[0-9a-f]{40}|sha256:[0-9a-f]{64})$'),
    state text NOT NULL CHECK(state IN ('pending','receiving','completed','cancelled')),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL,
    max_objects bigint NOT NULL CHECK(max_objects BETWEEN 1 AND 1000000),
    max_bytes bigint NOT NULL CHECK(max_bytes BETWEEN 22 AND 68719476736),
    native_commit bytea CHECK(octet_length(native_commit)=32),
    completed_at timestamptz,
    PRIMARY KEY(tenant,repository,import),
    FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
    CHECK(expires_at>created_at),
    CHECK(state NOT IN ('receiving','completed') OR native_commit IS NOT NULL),
    CHECK(state<>'pending' OR native_commit IS NULL),
    CHECK((state='completed')=(completed_at IS NOT NULL))
);
CREATE INDEX th_github_active_imports ON th_github_imports(tenant,repository,expires_at) WHERE state IN ('pending','receiving');
CREATE FUNCTION th_github_import_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.tenant,NEW.repository,NEW.import,NEW.binding,NEW.epoch,NEW.principal,NEW.bridge,NEW.git_revision,NEW.created_at,NEW.expires_at,NEW.max_objects,NEW.max_bytes)
       IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.import,OLD.binding,OLD.epoch,OLD.principal,OLD.bridge,OLD.git_revision,OLD.created_at,OLD.expires_at,OLD.max_objects,OLD.max_bytes)
       OR (OLD.native_commit IS NOT NULL AND NEW.native_commit IS DISTINCT FROM OLD.native_commit)
       OR (OLD.state IN ('completed','cancelled') AND NEW IS DISTINCT FROM OLD)
       OR NOT (NEW.state=OLD.state OR (OLD.state='pending' AND NEW.state IN ('receiving','cancelled')) OR (OLD.state='receiving' AND NEW.state IN ('completed','cancelled')))
    THEN RAISE EXCEPTION 'GitHub import context cannot be replaced' USING ERRCODE='23514'; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_import_update BEFORE UPDATE ON th_github_imports FOR EACH ROW EXECUTE FUNCTION th_github_import_immutable();
CREATE TRIGGER th_github_import_delete BEFORE DELETE ON th_github_imports FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

ALTER TABLE th_source_uploads ADD COLUMN github_import uuid;
ALTER TABLE th_source_uploads ADD FOREIGN KEY(tenant,repository,github_import) REFERENCES th_github_imports(tenant,repository,import);
ALTER TABLE th_source_uploads ADD CHECK(github_import IS NULL OR (github_import=upload AND authority='connected'));
CREATE FUNCTION th_source_upload_purpose_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.github_import IS DISTINCT FROM OLD.github_import THEN
      RAISE EXCEPTION 'source upload purpose cannot change' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_source_upload_purpose BEFORE UPDATE ON th_source_uploads FOR EACH ROW EXECUTE FUNCTION th_source_upload_purpose_immutable();
ALTER TABLE th_source_upload_items ADD COLUMN git_origin text;
ALTER TABLE th_source_upload_items ADD CHECK(git_origin IS NULL OR (kind=5 AND git_origin ~ '^git:(sha1:[0-9a-f]{40}|sha256:[0-9a-f]{64})$'));
