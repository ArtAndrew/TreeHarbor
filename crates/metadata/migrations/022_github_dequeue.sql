-- Old local cancellation flags deliberately do not create dequeue requests.
ALTER TABLE th_github_queue ADD COLUMN mutation_not_sent boolean NOT NULL DEFAULT false;
CREATE FUNCTION th_github_queue_no_send_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.mutation_not_sent AND NOT NEW.mutation_not_sent THEN
  RAISE EXCEPTION 'GitHub no-send evidence cannot be revoked' USING ERRCODE='23514';
 END IF;
 IF NEW.mutation_not_sent AND (NEW.attempt IS NULL OR NEW.report_sequence=0 OR NEW.provider_request IS NOT NULL OR NEW.dispatch_failed) THEN
  RAISE EXCEPTION 'GitHub no-send evidence contradicts dispatch evidence' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_queue_no_send_update BEFORE UPDATE ON th_github_queue FOR EACH ROW EXECUTE FUNCTION th_github_queue_no_send_immutable();
CREATE TABLE th_github_dequeues (
 tenant text NOT NULL, repository uuid NOT NULL, dequeue uuid NOT NULL UNIQUE,
 queue uuid NOT NULL, epoch bigint NOT NULL CHECK(epoch>0), principal text NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), expires_at timestamptz NOT NULL,
 attempt uuid, started_at timestamptz,
 report_sequence bigint NOT NULL DEFAULT 0 CHECK(report_sequence>=0), latest_observation jsonb,
 retained_receipt jsonb,
 mutation_not_sent boolean NOT NULL DEFAULT false,
 PRIMARY KEY(tenant,repository,dequeue),
 FOREIGN KEY(tenant,repository,queue) REFERENCES th_github_queue(tenant,repository,queue),
 CHECK(expires_at>created_at), CHECK((attempt IS NULL)=(started_at IS NULL)),
 CHECK((report_sequence=0)=(latest_observation IS NULL)),
 CHECK(report_sequence=0 OR attempt IS NOT NULL),
 CHECK(retained_receipt IS NULL OR report_sequence>0),
 CHECK(NOT mutation_not_sent OR (report_sequence>0 AND retained_receipt IS NULL))
);
CREATE INDEX th_github_dequeues_by_queue ON th_github_dequeues(tenant,repository,queue);
CREATE FUNCTION th_github_dequeue_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF ROW(NEW.tenant,NEW.repository,NEW.dequeue,NEW.queue,NEW.epoch,NEW.principal,NEW.created_at,NEW.expires_at)
 IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.dequeue,OLD.queue,OLD.epoch,OLD.principal,OLD.created_at,OLD.expires_at)
 OR (OLD.attempt IS NOT NULL AND ROW(NEW.attempt,NEW.started_at) IS DISTINCT FROM ROW(OLD.attempt,OLD.started_at))
 OR (OLD.retained_receipt IS NOT NULL AND NEW.retained_receipt IS DISTINCT FROM OLD.retained_receipt)
 OR (OLD.mutation_not_sent AND NOT NEW.mutation_not_sent)
 OR NEW.report_sequence NOT IN (OLD.report_sequence,OLD.report_sequence+1)
 OR ((NEW.report_sequence=OLD.report_sequence) AND NEW.latest_observation IS DISTINCT FROM OLD.latest_observation)
 THEN RAISE EXCEPTION 'GitHub dequeue identity or retained evidence cannot be replaced' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER th_github_dequeue_update BEFORE UPDATE ON th_github_dequeues FOR EACH ROW EXECUTE FUNCTION th_github_dequeue_immutable();
CREATE TRIGGER th_github_dequeue_delete BEFORE DELETE ON th_github_dequeues FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
CREATE TABLE th_github_dequeue_reports (
 tenant text NOT NULL, repository uuid NOT NULL, dequeue uuid NOT NULL, sequence bigint NOT NULL CHECK(sequence>0),
 report jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant,repository,dequeue,sequence),
 FOREIGN KEY(tenant,repository,dequeue) REFERENCES th_github_dequeues(tenant,repository,dequeue)
);
CREATE TRIGGER th_github_dequeue_report_update BEFORE UPDATE OR DELETE ON th_github_dequeue_reports FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
