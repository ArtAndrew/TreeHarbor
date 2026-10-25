-- New permission is opt-in; existing grants/tokens are not rewritten.
ALTER TABLE th_repository_access DROP CONSTRAINT th_repository_access_permissions_check;
ALTER TABLE th_repository_access ADD CHECK(permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','host.queue','host.merge','host.recover','host.check','review','policy.write','integration.control']::text[]);
ALTER TABLE th_access_tokens DROP CONSTRAINT th_access_tokens_permissions_check;
ALTER TABLE th_access_tokens ADD CHECK(cardinality(permissions)>0 AND permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','host.queue','host.merge','host.recover','host.check','review','policy.write','integration.control']::text[]);

CREATE TABLE th_github_check_publications (
 tenant text NOT NULL, repository uuid NOT NULL, publication uuid NOT NULL UNIQUE,
 check_run uuid NOT NULL, binding uuid NOT NULL, epoch bigint NOT NULL CHECK(epoch>0),
 principal text NOT NULL, token uuid NOT NULL, bridge text NOT NULL,
 target jsonb NOT NULL, head text NOT NULL CHECK(head ~ '^git:sha1:[0-9a-f]{40}$'),
 program bytea NOT NULL CHECK(octet_length(program)=32),
 state text NOT NULL CHECK(state IN ('requested','attempting','uncertain','observed','not_sent','cancelled')),
 cancel_requested boolean NOT NULL DEFAULT false,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), expires_at timestamptz NOT NULL,
 attempt uuid, attempt_token uuid, started_at timestamptz,
 preflight jsonb, dispatched_at timestamptz,
 report_sequence bigint NOT NULL DEFAULT 0 CHECK(report_sequence BETWEEN 0 AND 1024),
 latest_report jsonb,
 PRIMARY KEY(tenant,repository,publication),
 FOREIGN KEY(tenant,repository,check_run) REFERENCES th_github_managed_executions(tenant,repository,check_run),
 FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
 FOREIGN KEY(tenant,token) REFERENCES th_access_tokens(tenant,token),
 FOREIGN KEY(tenant,attempt_token) REFERENCES th_access_tokens(tenant,token),
 CHECK(expires_at>created_at),
 CHECK((attempt IS NULL)=(started_at IS NULL) AND (attempt IS NULL)=(attempt_token IS NULL)),
 CHECK(attempt IS NULL OR attempt<>'00000000-0000-0000-0000-000000000000'),
 CHECK((preflight IS NULL)=(dispatched_at IS NULL)),
 CHECK(preflight IS NULL OR attempt IS NOT NULL),
 CHECK((report_sequence=0)=(latest_report IS NULL)),
 CHECK(state NOT IN ('requested','cancelled') OR attempt IS NULL),
 CHECK(state<>'cancelled' OR cancel_requested),
 CHECK(state NOT IN ('attempting','uncertain','observed','not_sent') OR attempt IS NOT NULL),
 CHECK(state NOT IN ('uncertain','observed','not_sent') OR latest_report IS NOT NULL),
 CHECK(state<>'observed' OR preflight IS NOT NULL)
);
-- Cancellation/expiry after an uncertain write cannot free this fence.
CREATE UNIQUE INDEX th_github_check_publication_unresolved
 ON th_github_check_publications(tenant,repository,binding,epoch,head,program)
 WHERE state IN ('requested','attempting','uncertain');
CREATE FUNCTION th_github_check_publication_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF (to_jsonb(NEW)-ARRAY['state','cancel_requested','attempt','attempt_token','started_at','preflight','dispatched_at','report_sequence','latest_report']) IS DISTINCT FROM
    (to_jsonb(OLD)-ARRAY['state','cancel_requested','attempt','attempt_token','started_at','preflight','dispatched_at','report_sequence','latest_report'])
    OR (OLD.attempt IS NOT NULL AND ROW(NEW.attempt,NEW.attempt_token,NEW.started_at) IS DISTINCT FROM ROW(OLD.attempt,OLD.attempt_token,OLD.started_at))
    OR (OLD.preflight IS NOT NULL AND ROW(NEW.preflight,NEW.dispatched_at) IS DISTINCT FROM ROW(OLD.preflight,OLD.dispatched_at))
    OR (NEW.preflight IS DISTINCT FROM OLD.preflight AND (OLD.state<>'attempting' OR NEW.state<>'attempting'))
    OR (OLD.cancel_requested AND NOT NEW.cancel_requested)
    OR (OLD.state IN ('observed','not_sent','cancelled') AND NEW IS DISTINCT FROM OLD)
    OR NEW.report_sequence NOT IN (OLD.report_sequence,OLD.report_sequence+1)
    OR (NEW.report_sequence=OLD.report_sequence AND NEW.latest_report IS DISTINCT FROM OLD.latest_report)
    OR NOT (NEW.state=OLD.state
        OR (OLD.state='requested' AND NEW.state IN ('attempting','cancelled'))
        OR (OLD.state='attempting' AND NEW.state IN ('uncertain','observed','not_sent'))
        OR (OLD.state='uncertain' AND NEW.state='observed')) THEN
   RAISE EXCEPTION 'check publication identity or transition is immutable' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_check_publication_update BEFORE UPDATE ON th_github_check_publications
 FOR EACH ROW EXECUTE FUNCTION th_github_check_publication_identity();
CREATE TRIGGER th_github_check_publication_delete BEFORE DELETE ON th_github_check_publications
 FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_github_check_publication_reports (
 tenant text NOT NULL, repository uuid NOT NULL, publication uuid NOT NULL,
 sequence bigint NOT NULL CHECK(sequence BETWEEN 1 AND 1024),
 attempt uuid NOT NULL, reporter text NOT NULL, token uuid NOT NULL, report jsonb NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant,repository,publication,sequence),
 FOREIGN KEY(tenant,repository,publication) REFERENCES th_github_check_publications(tenant,repository,publication),
 FOREIGN KEY(tenant,token) REFERENCES th_access_tokens(tenant,token)
);
CREATE TRIGGER th_github_check_publication_report_immutable BEFORE UPDATE OR DELETE ON th_github_check_publication_reports
 FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
