CREATE TABLE th_github_projections (
  tenant text NOT NULL,
  repository uuid NOT NULL,
  projection uuid NOT NULL UNIQUE,
  binding uuid NOT NULL,
  epoch bigint NOT NULL CHECK(epoch>0),
  principal text NOT NULL,
  changeset uuid NOT NULL,
  version bigint NOT NULL,
  title text NOT NULL CHECK(octet_length(title) BETWEEN 1 AND 256),
  native_commit bytea NOT NULL CHECK(octet_length(native_commit)=32),
  state text NOT NULL CHECK(state IN ('pending','ready','cancelled')),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  expires_at timestamptz NOT NULL,
  backend text,
  generation bigint,
  hold_token uuid,
  pin_holder text,
  PRIMARY KEY(tenant,repository,projection),
  FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
  FOREIGN KEY(tenant,repository,changeset,version) REFERENCES th_change_versions(tenant,repository,changeset,version),
  FOREIGN KEY(tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder),
  CHECK(expires_at>created_at),
  CHECK((backend IS NULL)=(generation IS NULL) AND (backend IS NULL)=(hold_token IS NULL)),
  CHECK(state<>'ready' OR (backend IS NOT NULL AND pin_holder IS NOT NULL)),
  CHECK(state<>'pending' OR pin_holder IS NULL)
);
CREATE INDEX th_github_projection_pending ON th_github_projections(tenant,repository,expires_at) WHERE state='pending';
CREATE FUNCTION th_github_projection_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF ROW(NEW.tenant,NEW.repository,NEW.projection,NEW.binding,NEW.epoch,NEW.principal,NEW.changeset,NEW.version,NEW.title,NEW.native_commit,NEW.created_at,NEW.expires_at)
     IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.projection,OLD.binding,OLD.epoch,OLD.principal,OLD.changeset,OLD.version,OLD.title,OLD.native_commit,OLD.created_at,OLD.expires_at)
     OR (OLD.backend IS NOT NULL AND ROW(NEW.backend,NEW.generation,NEW.hold_token) IS DISTINCT FROM ROW(OLD.backend,OLD.generation,OLD.hold_token))
     OR (OLD.pin_holder IS NOT NULL AND NEW.pin_holder IS DISTINCT FROM OLD.pin_holder)
     OR (OLD.state='cancelled' AND NEW IS DISTINCT FROM OLD)
     OR NOT (NEW.state=OLD.state OR (OLD.state='pending' AND NEW.state IN ('ready','cancelled')) OR (OLD.state='ready' AND NEW.state='cancelled'))
  THEN RAISE EXCEPTION 'GitHub projection cannot be replaced' USING ERRCODE='23514'; END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_projection_update BEFORE UPDATE ON th_github_projections FOR EACH ROW EXECUTE FUNCTION th_github_projection_immutable();
CREATE TRIGGER th_github_projection_delete BEFORE DELETE ON th_github_projections FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
