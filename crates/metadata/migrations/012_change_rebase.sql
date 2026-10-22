CREATE TABLE th_change_rebases (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    rebase uuid NOT NULL,
    principal text NOT NULL,
    changeset uuid NOT NULL,
    source_version bigint NOT NULL CHECK(source_version>0 AND source_version<9223372036854775807),
    onto text NOT NULL,
    epoch bigint NOT NULL CHECK(epoch>0),
    profile text NOT NULL,
    timestamp_seconds bigint NOT NULL,
    state text NOT NULL CHECK(state IN ('preparing','ready','conflicted','failed','cancelled')),
    result_version bigint,
    revision bytea CHECK(octet_length(revision)=32),
    failure jsonb CHECK(failure IS NULL OR octet_length(failure::text)<=65536),
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,rebase),
    FOREIGN KEY(tenant,repository,changeset,source_version) REFERENCES th_change_versions(tenant,repository,changeset,version),
    FOREIGN KEY(tenant,repository,onto) REFERENCES th_bases(tenant,repository,revision),
    FOREIGN KEY(tenant,repository,changeset,result_version) REFERENCES th_change_versions(tenant,repository,changeset,version),
    CHECK((state='ready' AND result_version IS NOT NULL AND result_version=source_version+1 AND revision IS NOT NULL) OR
          (state<>'ready' AND result_version IS NULL AND revision IS NULL))
);
CREATE TRIGGER th_rebase_input_immutable BEFORE UPDATE OF tenant,repository,rebase,principal,changeset,source_version,onto,epoch,profile,timestamp_seconds,expires_at ON th_change_rebases
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_change_rebase_objects (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    rebase uuid NOT NULL,
    digest bytea NOT NULL CHECK(octet_length(digest)=32),
    generation bigint NOT NULL CHECK(generation>0),
    hold_token uuid NOT NULL,
    length bigint NOT NULL CHECK(length>0),
    backend text NOT NULL,
    PRIMARY KEY(tenant,repository,rebase,digest),
    FOREIGN KEY(tenant,repository,rebase) REFERENCES th_change_rebases(tenant,repository,rebase)
);
CREATE TABLE th_change_rebase_lineage (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    changeset uuid NOT NULL,
    result_version bigint NOT NULL,
    source_version bigint NOT NULL,
    rebase uuid NOT NULL,
    revision bytea NOT NULL CHECK(octet_length(revision)=32),
    pin_holder text NOT NULL,
    PRIMARY KEY(tenant,repository,changeset,result_version),
    UNIQUE(tenant,repository,rebase),
    CHECK(result_version=source_version+1),
    FOREIGN KEY(tenant,repository,changeset,result_version) REFERENCES th_change_versions(tenant,repository,changeset,version),
    FOREIGN KEY(tenant,repository,changeset,source_version) REFERENCES th_change_versions(tenant,repository,changeset,version),
    FOREIGN KEY(tenant,repository,rebase) REFERENCES th_change_rebases(tenant,repository,rebase),
    FOREIGN KEY(tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder)
);
CREATE TRIGGER th_rebase_lineage_immutable BEFORE UPDATE OR DELETE ON th_change_rebase_lineage
    FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
