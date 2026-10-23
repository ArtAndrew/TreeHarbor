ALTER TABLE th_repository_access DROP CONSTRAINT th_repository_access_permissions_check;
ALTER TABLE th_repository_access ADD CHECK(permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','host.queue','host.merge','review','policy.write','integration.control']::text[]);
ALTER TABLE th_access_tokens DROP CONSTRAINT th_access_tokens_permissions_check;
ALTER TABLE th_access_tokens ADD CHECK(cardinality(permissions)>0 AND permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','host.queue','host.merge','review','policy.write','integration.control']::text[]);

CREATE TABLE th_github_queue (
 tenant text NOT NULL, repository uuid NOT NULL, queue uuid NOT NULL UNIQUE,
 inspection uuid NOT NULL, pull uuid NOT NULL, projection uuid NOT NULL, binding uuid NOT NULL,
 epoch bigint NOT NULL CHECK(epoch>0), principal text NOT NULL, bridge text NOT NULL,
 native_commit bytea NOT NULL CHECK(octet_length(native_commit)=32), target jsonb NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), expires_at timestamptz NOT NULL,
 attempt uuid, started_at timestamptz, cancel_requested boolean NOT NULL DEFAULT false,
 report_sequence bigint NOT NULL DEFAULT 0 CHECK(report_sequence>=0), latest_observation jsonb,
 provider_request uuid, dispatch_failed boolean NOT NULL DEFAULT false, pin_holder text NOT NULL,
 PRIMARY KEY(tenant,repository,queue),
 FOREIGN KEY(tenant,repository,inspection) REFERENCES th_github_inspections(tenant,repository,inspection),
 FOREIGN KEY(tenant,repository,pull) REFERENCES th_github_pulls(tenant,repository,pull),
 FOREIGN KEY(tenant,repository,projection) REFERENCES th_github_projections(tenant,repository,projection),
 FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
 FOREIGN KEY(tenant,repository,pin_holder) REFERENCES th_object_pins(tenant,repository,holder),
 CHECK(expires_at>created_at), CHECK((attempt IS NULL)=(started_at IS NULL)),
 CHECK((report_sequence=0)=(latest_observation IS NULL)),
 CHECK(report_sequence=0 OR attempt IS NOT NULL),
 CHECK(provider_request IS NULL OR attempt IS NOT NULL),
 CHECK(NOT dispatch_failed OR report_sequence>0)
);
CREATE UNIQUE INDEX th_github_queue_pending ON th_github_queue(tenant,repository,pull) WHERE NOT cancel_requested AND NOT dispatch_failed;
CREATE FUNCTION th_github_queue_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF ROW(NEW.tenant,NEW.repository,NEW.queue,NEW.inspection,NEW.pull,NEW.projection,NEW.binding,NEW.epoch,NEW.principal,NEW.bridge,NEW.native_commit,NEW.target,NEW.created_at,NEW.expires_at,NEW.pin_holder)
 IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.queue,OLD.inspection,OLD.pull,OLD.projection,OLD.binding,OLD.epoch,OLD.principal,OLD.bridge,OLD.native_commit,OLD.target,OLD.created_at,OLD.expires_at,OLD.pin_holder)
 OR (OLD.attempt IS NOT NULL AND ROW(NEW.attempt,NEW.started_at) IS DISTINCT FROM ROW(OLD.attempt,OLD.started_at))
 OR (OLD.provider_request IS NOT NULL AND NEW.provider_request IS DISTINCT FROM OLD.provider_request)
 OR (OLD.cancel_requested AND NOT NEW.cancel_requested)
 OR (OLD.dispatch_failed AND NOT NEW.dispatch_failed)
 OR NEW.report_sequence NOT IN (OLD.report_sequence,OLD.report_sequence+1)
 OR ((NEW.report_sequence=OLD.report_sequence) AND NEW.latest_observation IS DISTINCT FROM OLD.latest_observation)
 THEN RAISE EXCEPTION 'GitHub queue identity or retained observation cannot be replaced' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_queue_update BEFORE UPDATE ON th_github_queue FOR EACH ROW EXECUTE FUNCTION th_github_queue_immutable();
CREATE TRIGGER th_github_queue_delete BEFORE DELETE ON th_github_queue FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_github_queue_reports (
 tenant text NOT NULL, repository uuid NOT NULL, queue uuid NOT NULL, sequence bigint NOT NULL CHECK(sequence>0),
 report jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant,repository,queue,sequence),
 FOREIGN KEY(tenant,repository,queue) REFERENCES th_github_queue(tenant,repository,queue)
);
CREATE TRIGGER th_github_queue_report_update BEFORE UPDATE OR DELETE ON th_github_queue_reports FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
