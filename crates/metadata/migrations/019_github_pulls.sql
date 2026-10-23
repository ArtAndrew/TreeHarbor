CREATE TABLE th_github_pulls (
  tenant text NOT NULL, repository uuid NOT NULL, pull uuid NOT NULL UNIQUE,
  publication uuid NOT NULL, projection uuid NOT NULL, binding uuid NOT NULL,
  epoch bigint NOT NULL CHECK(epoch>0), principal text NOT NULL, bridge text NOT NULL,
  branch uuid NOT NULL, native_commit bytea NOT NULL CHECK(octet_length(native_commit)=32),
  git_revision text NOT NULL CHECK(git_revision ~ '^git:sha1:[0-9a-f]{40}$'),
  target jsonb NOT NULL, previous_pull uuid,
  state text NOT NULL CHECK(state IN ('requested','attempting','uncertain','observed','cancelled')),
  cancel_requested boolean NOT NULL DEFAULT false,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(), expires_at timestamptz NOT NULL,
  attempt uuid, started_at timestamptz, observation jsonb,
  pull_number bigint CHECK(pull_number>0), pull_node text,
  pin_holder text NOT NULL,
  PRIMARY KEY(tenant,repository,pull),
  FOREIGN KEY(tenant,repository,publication) REFERENCES th_github_publications(tenant,repository,publication),
  FOREIGN KEY(tenant,repository,projection) REFERENCES th_github_projections(tenant,repository,projection),
  FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
  FOREIGN KEY(tenant,repository,previous_pull) REFERENCES th_github_pulls(tenant,repository,pull),
  FOREIGN KEY(tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder),
  CHECK(expires_at>created_at), CHECK((attempt IS NULL)=(started_at IS NULL)),
  CHECK((pull_number IS NULL)=(pull_node IS NULL)),
  CHECK(state<>'requested' OR attempt IS NULL),
  CHECK(state NOT IN ('attempting','uncertain','observed') OR attempt IS NOT NULL),
  CHECK(state<>'cancelled' OR (cancel_requested AND attempt IS NULL)),
  CHECK(state NOT IN ('uncertain','observed') OR observation IS NOT NULL),
  CHECK(state<>'observed' OR pull_number IS NOT NULL)
);
CREATE UNIQUE INDEX th_github_pull_unresolved ON th_github_pulls(tenant,repository,branch)
  WHERE state IN ('requested','attempting','uncertain');
CREATE FUNCTION th_github_pull_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF ROW(NEW.tenant,NEW.repository,NEW.pull,NEW.publication,NEW.projection,NEW.binding,NEW.epoch,NEW.principal,NEW.bridge,NEW.branch,NEW.native_commit,NEW.git_revision,NEW.target,NEW.previous_pull,NEW.created_at,NEW.expires_at,NEW.pin_holder)
     IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.pull,OLD.publication,OLD.projection,OLD.binding,OLD.epoch,OLD.principal,OLD.bridge,OLD.branch,OLD.native_commit,OLD.git_revision,OLD.target,OLD.previous_pull,OLD.created_at,OLD.expires_at,OLD.pin_holder)
     OR (OLD.attempt IS NOT NULL AND ROW(NEW.attempt,NEW.started_at) IS DISTINCT FROM ROW(OLD.attempt,OLD.started_at))
     OR (OLD.pull_number IS NOT NULL AND ROW(NEW.pull_number,NEW.pull_node) IS DISTINCT FROM ROW(OLD.pull_number,OLD.pull_node))
     OR (OLD.cancel_requested AND NOT NEW.cancel_requested)
     OR (OLD.state IN ('observed','cancelled') AND NEW IS DISTINCT FROM OLD)
     OR NOT (NEW.state=OLD.state OR (OLD.state='requested' AND NEW.state IN ('attempting','cancelled'))
       OR (OLD.state='attempting' AND NEW.state IN ('uncertain','observed'))
       OR (OLD.state='uncertain' AND NEW.state='observed'))
  THEN RAISE EXCEPTION 'GitHub pull identity or transition cannot be replaced' USING ERRCODE='23514'; END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_pull_update BEFORE UPDATE ON th_github_pulls FOR EACH ROW EXECUTE FUNCTION th_github_pull_immutable();
CREATE TRIGGER th_github_pull_delete BEFORE DELETE ON th_github_pulls FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
