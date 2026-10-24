-- Signed facts are retained independently of arrival order. They authorize no merge.
CREATE TABLE th_github_event_channels (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    channel uuid NOT NULL UNIQUE,
    binding uuid NOT NULL,
    epoch bigint NOT NULL CHECK(epoch>0),
    app_id bigint NOT NULL CHECK(app_id>0),
    installation_id bigint NOT NULL CHECK(installation_id>0),
    repository_id bigint NOT NULL CHECK(repository_id>0),
    owner_id bigint NOT NULL CHECK(owner_id>0),
    principal text NOT NULL,
    state text NOT NULL CHECK(state IN ('active','revoked')),
    max_events bigint NOT NULL CHECK(max_events BETWEEN 1 AND 1000000),
    max_bytes bigint NOT NULL CHECK(max_bytes BETWEEN 1 AND 10737418240),
    max_aliases integer NOT NULL CHECK(max_aliases BETWEEN 1 AND 16),
    retained_events bigint NOT NULL DEFAULT 0 CHECK(retained_events BETWEEN 0 AND max_events),
    retained_bytes bigint NOT NULL DEFAULT 0 CHECK(retained_bytes BETWEEN 0 AND max_bytes),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    revoked_at timestamptz,
    PRIMARY KEY(tenant,repository,channel),
    FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
    CHECK((state='revoked')=(revoked_at IS NOT NULL))
);
CREATE UNIQUE INDEX th_github_event_route ON th_github_event_channels(app_id,installation_id,repository_id) WHERE state='active';
CREATE FUNCTION th_github_event_channel_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.tenant,NEW.repository,NEW.channel,NEW.binding,NEW.epoch,NEW.app_id,NEW.installation_id,NEW.repository_id,NEW.owner_id,NEW.principal,NEW.max_events,NEW.max_bytes,NEW.max_aliases,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.channel,OLD.binding,OLD.epoch,OLD.app_id,OLD.installation_id,OLD.repository_id,OLD.owner_id,OLD.principal,OLD.max_events,OLD.max_bytes,OLD.max_aliases,OLD.created_at)
       OR (OLD.state='revoked' AND NEW IS DISTINCT FROM OLD)
       OR NOT (NEW.state=OLD.state OR (OLD.state='active' AND NEW.state='revoked'))
       OR NEW.retained_events<OLD.retained_events OR NEW.retained_bytes<OLD.retained_bytes
    THEN RAISE EXCEPTION 'event channel identity cannot be replaced' USING ERRCODE='23514'; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_event_channel_update BEFORE UPDATE ON th_github_event_channels FOR EACH ROW EXECUTE FUNCTION th_github_event_channel_immutable();
CREATE TRIGGER th_github_event_channel_delete BEFORE DELETE ON th_github_event_channels FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

CREATE TABLE th_github_events (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant text NOT NULL,
    repository uuid NOT NULL,
    channel uuid NOT NULL,
    event uuid NOT NULL UNIQUE,
    key_id uuid NOT NULL,
    digest bytea NOT NULL CHECK(octet_length(digest)=32),
    body bytea NOT NULL CHECK(octet_length(body) BETWEEN 1 AND 1048576),
    normalized jsonb NOT NULL CHECK(jsonb_typeof(normalized)='object'),
    received_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE(tenant,repository,channel,digest),
    FOREIGN KEY(tenant,repository,channel) REFERENCES th_github_event_channels(tenant,repository,channel)
);
CREATE INDEX th_github_events_page ON th_github_events(tenant,repository,channel,id);
CREATE TRIGGER th_github_event_immutable BEFORE UPDATE OR DELETE ON th_github_events FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_github_event_deliveries (
    tenant text NOT NULL,
    repository uuid NOT NULL,
    channel uuid NOT NULL,
    delivery uuid NOT NULL,
    digest bytea NOT NULL,
    key_id uuid NOT NULL,
    received_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(tenant,repository,channel,delivery),
    FOREIGN KEY(tenant,repository,channel,digest) REFERENCES th_github_events(tenant,repository,channel,digest)
);
CREATE INDEX th_github_event_aliases ON th_github_event_deliveries(tenant,repository,channel,digest);
CREATE TRIGGER th_github_event_delivery_immutable BEFORE UPDATE OR DELETE ON th_github_event_deliveries FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
