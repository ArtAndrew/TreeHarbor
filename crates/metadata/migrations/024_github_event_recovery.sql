-- New capability is opt-in. Existing grants and tokens remain byte-for-byte intact.
ALTER TABLE th_repository_access DROP CONSTRAINT th_repository_access_permissions_check;
ALTER TABLE th_repository_access ADD CHECK(permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','host.queue','host.merge','host.recover','review','policy.write','integration.control']::text[]);
ALTER TABLE th_access_tokens DROP CONSTRAINT th_access_tokens_permissions_check;
ALTER TABLE th_access_tokens ADD CHECK(cardinality(permissions)>0 AND permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','host.queue','host.merge','host.recover','review','policy.write','integration.control']::text[]);

CREATE TABLE th_github_event_recoveries (
 tenant text NOT NULL, repository uuid NOT NULL, recovery uuid NOT NULL UNIQUE,
 channel uuid NOT NULL, binding uuid NOT NULL, epoch bigint NOT NULL CHECK(epoch>0),
 principal text NOT NULL, bridge text NOT NULL, callback_url text NOT NULL CHECK(octet_length(callback_url) BETWEEN 1 AND 2048),
 since_unix_ms bigint NOT NULL CHECK(since_unix_ms>0),
 max_pages integer NOT NULL CHECK(max_pages BETWEEN 1 AND 100),
 max_deliveries integer NOT NULL CHECK(max_deliveries BETWEEN 1 AND 100),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), expires_at timestamptz NOT NULL,
 cancelled boolean NOT NULL DEFAULT false,
 page_count integer NOT NULL DEFAULT 0 CHECK(page_count BETWEEN 0 AND max_pages),
 scanned_count integer NOT NULL DEFAULT 0 CHECK(scanned_count BETWEEN 0 AND max_pages*100),
 detail_count integer NOT NULL DEFAULT 0 CHECK(detail_count BETWEEN 0 AND max_deliveries),
 dispatch_count integer NOT NULL DEFAULT 0 CHECK(dispatch_count BETWEEN 0 AND detail_count),
 next_cursor text CHECK(next_cursor IS NULL OR octet_length(next_cursor) BETWEEN 1 AND 1024),
 scan_exhausted boolean NOT NULL DEFAULT false,
 PRIMARY KEY(tenant,repository,recovery),
 FOREIGN KEY(tenant,repository,channel) REFERENCES th_github_event_channels(tenant,repository,channel),
 FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
 CHECK(expires_at>created_at), CHECK(NOT scan_exhausted OR (page_count>0 AND next_cursor IS NULL))
);
CREATE FUNCTION th_github_recovery_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF ROW(NEW.tenant,NEW.repository,NEW.recovery,NEW.channel,NEW.binding,NEW.epoch,NEW.principal,NEW.bridge,NEW.callback_url,NEW.since_unix_ms,NEW.max_pages,NEW.max_deliveries,NEW.created_at,NEW.expires_at)
 IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.recovery,OLD.channel,OLD.binding,OLD.epoch,OLD.principal,OLD.bridge,OLD.callback_url,OLD.since_unix_ms,OLD.max_pages,OLD.max_deliveries,OLD.created_at,OLD.expires_at)
 OR (OLD.cancelled AND NOT NEW.cancelled) OR (OLD.scan_exhausted AND NOT NEW.scan_exhausted)
 OR NEW.page_count<OLD.page_count OR NEW.scanned_count<OLD.scanned_count OR NEW.detail_count<OLD.detail_count OR NEW.dispatch_count<OLD.dispatch_count
 THEN RAISE EXCEPTION 'recovery identity or monotonic state changed' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER th_github_recovery_update BEFORE UPDATE ON th_github_event_recoveries FOR EACH ROW EXECUTE FUNCTION th_github_recovery_immutable();
CREATE TRIGGER th_github_recovery_delete BEFORE DELETE ON th_github_event_recoveries FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

CREATE TABLE th_github_event_recovery_pages (
 tenant text NOT NULL, repository uuid NOT NULL, recovery uuid NOT NULL, sequence integer NOT NULL CHECK(sequence BETWEEN 1 AND 100),
 report jsonb NOT NULL CHECK(jsonb_typeof(report)='object'),
 PRIMARY KEY(tenant,repository,recovery,sequence),
 FOREIGN KEY(tenant,repository,recovery) REFERENCES th_github_event_recoveries(tenant,repository,recovery)
);
CREATE UNIQUE INDEX th_github_recovery_cursor ON th_github_event_recovery_pages(tenant,repository,recovery,(coalesce(report->>'cursor','')));
CREATE TRIGGER th_github_recovery_page_immutable BEFORE UPDATE OR DELETE ON th_github_event_recovery_pages FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

CREATE TABLE th_github_event_recovery_attempts (
 tenant text NOT NULL, repository uuid NOT NULL, recovery uuid NOT NULL, provider_id bigint NOT NULL CHECK(provider_id>0),
 delivery uuid NOT NULL, observation jsonb NOT NULL CHECK(jsonb_typeof(observation)='object'),
 details jsonb CHECK(details IS NULL OR jsonb_typeof(details)='object'),
 dispatch uuid UNIQUE, started_at timestamptz,
 report jsonb CHECK(report IS NULL OR jsonb_typeof(report)='object'),
 PRIMARY KEY(tenant,repository,recovery,provider_id),
 FOREIGN KEY(tenant,repository,recovery) REFERENCES th_github_event_recoveries(tenant,repository,recovery),
 CHECK((dispatch IS NULL)=(started_at IS NULL)),
 CHECK(dispatch IS NULL OR details IS NOT NULL), CHECK(report IS NULL OR dispatch IS NOT NULL)
);
CREATE FUNCTION th_github_recovery_attempt_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF ROW(NEW.tenant,NEW.repository,NEW.recovery,NEW.provider_id,NEW.delivery,NEW.observation)
 IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.recovery,OLD.provider_id,OLD.delivery,OLD.observation)
 OR (OLD.details IS NOT NULL AND NEW.details IS DISTINCT FROM OLD.details)
 OR (OLD.dispatch IS NOT NULL AND ROW(NEW.dispatch,NEW.started_at) IS DISTINCT FROM ROW(OLD.dispatch,OLD.started_at))
 OR (OLD.report IS NOT NULL AND NEW.report IS DISTINCT FROM OLD.report)
 THEN RAISE EXCEPTION 'recovery attempt is append-only' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER th_github_recovery_attempt_update BEFORE UPDATE ON th_github_event_recovery_attempts FOR EACH ROW EXECUTE FUNCTION th_github_recovery_attempt_immutable();
CREATE TRIGGER th_github_recovery_attempt_delete BEFORE DELETE ON th_github_event_recovery_attempts FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
