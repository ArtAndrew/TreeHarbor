CREATE TABLE th_archive_pull_origins (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    changeset uuid NOT NULL,
    stage_key text NOT NULL,
    pull_number bigint NOT NULL CHECK (pull_number>0),
    origin jsonb NOT NULL CHECK (jsonb_typeof(origin)='object' AND octet_length(origin::text)<=65536),
    PRIMARY KEY (tenant,repository,changeset),
    UNIQUE (tenant,repository,stage_key,pull_number),
    FOREIGN KEY (tenant,repository,changeset) REFERENCES th_changesets(tenant,repository,changeset),
    FOREIGN KEY (tenant,repository,stage_key) REFERENCES th_operations(tenant,repository,key)
);
CREATE TRIGGER th_archive_pull_origin_immutable BEFORE UPDATE OR DELETE ON th_archive_pull_origins
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

ALTER TABLE th_change_versions ALTER COLUMN run DROP NOT NULL;
ALTER TABLE th_change_versions ADD COLUMN archived_pull uuid;
ALTER TABLE th_change_versions ADD CONSTRAINT th_change_version_source_check
    CHECK ((run IS NULL) <> (archived_pull IS NULL) AND (archived_pull IS NULL OR archived_pull=changeset));
ALTER TABLE th_change_versions ADD FOREIGN KEY (tenant,repository,archived_pull)
    REFERENCES th_archive_pull_origins(tenant,repository,changeset);
