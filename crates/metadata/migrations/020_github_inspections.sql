CREATE TABLE th_github_inspections (
  tenant text NOT NULL, repository uuid NOT NULL, inspection uuid NOT NULL UNIQUE,
  pull uuid NOT NULL, projection uuid NOT NULL, binding uuid NOT NULL,
  epoch bigint NOT NULL CHECK(epoch>0), principal text NOT NULL, bridge text NOT NULL,
  native_commit bytea NOT NULL CHECK(octet_length(native_commit)=32), target jsonb NOT NULL,
  state text NOT NULL CHECK(state IN ('requested','observed','cancelled')),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(), expires_at timestamptz NOT NULL,
  report jsonb, pin_holder text NOT NULL,
  PRIMARY KEY(tenant,repository,inspection),
  FOREIGN KEY(tenant,repository,pull) REFERENCES th_github_pulls(tenant,repository,pull),
  FOREIGN KEY(tenant,repository,projection) REFERENCES th_github_projections(tenant,repository,projection),
  FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
  FOREIGN KEY(tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder),
  CHECK(expires_at>created_at), CHECK((state='observed')=(report IS NOT NULL))
);
CREATE FUNCTION th_github_inspection_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF ROW(NEW.tenant,NEW.repository,NEW.inspection,NEW.pull,NEW.projection,NEW.binding,NEW.epoch,NEW.principal,NEW.bridge,NEW.native_commit,NEW.target,NEW.created_at,NEW.expires_at,NEW.pin_holder)
     IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.inspection,OLD.pull,OLD.projection,OLD.binding,OLD.epoch,OLD.principal,OLD.bridge,OLD.native_commit,OLD.target,OLD.created_at,OLD.expires_at,OLD.pin_holder)
     OR (OLD.state<>'requested' AND NEW IS DISTINCT FROM OLD)
     OR NOT (NEW.state=OLD.state OR (OLD.state='requested' AND NEW.state IN ('observed','cancelled')))
  THEN RAISE EXCEPTION 'GitHub inspection identity or observation cannot be replaced' USING ERRCODE='23514'; END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_inspection_update BEFORE UPDATE ON th_github_inspections FOR EACH ROW EXECUTE FUNCTION th_github_inspection_immutable();
CREATE TRIGGER th_github_inspection_delete BEFORE DELETE ON th_github_inspections FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
