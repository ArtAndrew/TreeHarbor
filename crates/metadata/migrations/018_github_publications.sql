CREATE TABLE th_github_publications (
  tenant text NOT NULL,
  repository uuid NOT NULL,
  publication uuid NOT NULL UNIQUE,
  projection uuid NOT NULL,
  binding uuid NOT NULL,
  epoch bigint NOT NULL CHECK(epoch>0),
  principal text NOT NULL,
  bridge text NOT NULL,
  branch uuid NOT NULL CHECK(branch<>'00000000-0000-0000-0000-000000000000'),
  native_commit bytea NOT NULL CHECK(octet_length(native_commit)=32),
  expected_git text CHECK(expected_git ~ '^git:sha1:[0-9a-f]{40}$'),
  prepared_git text CHECK(prepared_git ~ '^git:sha1:[0-9a-f]{40}$'),
  state text NOT NULL CHECK(state IN ('requested','prepared','attempting','uncertain','observed','not_sent','cancelled')),
  cancel_requested boolean NOT NULL DEFAULT false,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  expires_at timestamptz NOT NULL,
  attempt uuid,
  started_at timestamptz,
  observation jsonb,
  pin_holder text NOT NULL,
  PRIMARY KEY(tenant,repository,publication),
  FOREIGN KEY(tenant,repository,projection) REFERENCES th_github_projections(tenant,repository,projection),
  FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
  FOREIGN KEY(tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder),
  CHECK(expires_at>created_at),
  CHECK((attempt IS NULL)=(started_at IS NULL)),
  CHECK(attempt IS NULL OR prepared_git IS NOT NULL),
  CHECK(state<>'requested' OR (prepared_git IS NULL AND attempt IS NULL)),
  CHECK(state<>'prepared' OR (prepared_git IS NOT NULL AND attempt IS NULL)),
  CHECK(state NOT IN ('attempting','uncertain','observed','not_sent') OR attempt IS NOT NULL),
  CHECK(state<>'cancelled' OR (cancel_requested AND attempt IS NULL)),
  CHECK(state NOT IN ('uncertain','observed','not_sent') OR observation IS NOT NULL)
);
CREATE UNIQUE INDEX th_github_publication_unresolved ON th_github_publications(tenant,repository,branch)
  WHERE state IN ('requested','prepared','attempting','uncertain');
CREATE FUNCTION th_github_publication_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF ROW(NEW.tenant,NEW.repository,NEW.publication,NEW.projection,NEW.binding,NEW.epoch,NEW.principal,NEW.bridge,NEW.branch,NEW.native_commit,NEW.expected_git,NEW.created_at,NEW.expires_at,NEW.pin_holder)
     IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.publication,OLD.projection,OLD.binding,OLD.epoch,OLD.principal,OLD.bridge,OLD.branch,OLD.native_commit,OLD.expected_git,OLD.created_at,OLD.expires_at,OLD.pin_holder)
     OR (OLD.prepared_git IS NOT NULL AND NEW.prepared_git IS DISTINCT FROM OLD.prepared_git)
     OR (OLD.attempt IS NOT NULL AND ROW(NEW.attempt,NEW.started_at) IS DISTINCT FROM ROW(OLD.attempt,OLD.started_at))
     OR (OLD.cancel_requested AND NOT NEW.cancel_requested)
     OR (OLD.state IN ('observed','not_sent','cancelled') AND NEW IS DISTINCT FROM OLD)
     OR NOT (NEW.state=OLD.state
       OR (OLD.state='requested' AND NEW.state IN ('prepared','cancelled'))
       OR (OLD.state='prepared' AND NEW.state IN ('attempting','cancelled'))
       OR (OLD.state='attempting' AND NEW.state IN ('uncertain','observed','not_sent'))
       OR (OLD.state='uncertain' AND NEW.state='observed'))
  THEN RAISE EXCEPTION 'GitHub publication identity or transition cannot be replaced' USING ERRCODE='23514'; END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_publication_update BEFORE UPDATE ON th_github_publications FOR EACH ROW EXECUTE FUNCTION th_github_publication_immutable();
CREATE TRIGGER th_github_publication_delete BEFORE DELETE ON th_github_publications FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
