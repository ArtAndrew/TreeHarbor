-- Source delivery and execution provenance are independent of repository authority.
-- Existing trusted coordinator records are not retroactively labelled managed.
ALTER TABLE th_workspaces ADD COLUMN delivery text NOT NULL DEFAULT 'files'
    CHECK (delivery IN ('files','api'));
ALTER TABLE th_workspaces DROP CONSTRAINT th_workspaces_state_check;
ALTER TABLE th_workspaces ADD CONSTRAINT th_workspaces_state_check CHECK (
    (delivery='files' AND state IN ('requested','ready','in_use','draining','deleted')) OR
    (delivery='api' AND state IN ('source_ready','deleted') AND format='native'
     AND node IS NULL AND runtime IS NULL
     AND ((state='source_ready' AND pin_holder IS NOT NULL) OR (state='deleted' AND pin_holder IS NULL))));

ALTER TABLE th_agent_runs ADD COLUMN origin text NOT NULL DEFAULT 'control_plane'
    CHECK (origin IN ('control_plane','external_api'));
ALTER TABLE th_agent_runs DROP CONSTRAINT th_agent_runs_state_check;
ALTER TABLE th_agent_runs ADD CONSTRAINT th_agent_runs_state_check CHECK (
    (origin='control_plane' AND state IN ('requested','launching','running','stopping','succeeded','failed','cancelled','lost')) OR
    (origin='external_api' AND state IN ('recorded','closed') AND runtime IS NULL AND started_at IS NULL));
DROP INDEX th_one_active_run;
CREATE UNIQUE INDEX th_one_active_run ON th_agent_runs(tenant,repository,workspace)
    WHERE state IN ('requested','launching','running','stopping','recorded');

CREATE FUNCTION th_workspace_delivery_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.delivery IS DISTINCT FROM OLD.delivery OR
       (OLD.delivery='api' AND
        (ROW(NEW.tenant,NEW.repository,NEW.workspace,NEW.base_revision,NEW.principal,NEW.profile,
             NEW.format,NEW.epoch,NEW.resolved_profile,NEW.created_at)
         IS DISTINCT FROM
         ROW(OLD.tenant,OLD.repository,OLD.workspace,OLD.base_revision,OLD.principal,OLD.profile,
             OLD.format,OLD.epoch,OLD.resolved_profile,OLD.created_at)
         OR (OLD.state='deleted' AND NEW IS DISTINCT FROM OLD)
         OR (NEW IS DISTINCT FROM OLD AND NOT
             (OLD.state='source_ready' AND NEW.state='deleted'
              AND NEW.generation=OLD.generation+1 AND NEW.pin_holder IS NULL))))
    THEN
        RAISE EXCEPTION 'TreeHarbor workspace delivery or API identity cannot be replaced' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_workspace_delivery_identity BEFORE UPDATE ON th_workspaces
    FOR EACH ROW EXECUTE FUNCTION th_workspace_delivery_identity();

CREATE FUNCTION th_api_agent_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.origin IS DISTINCT FROM OLD.origin OR
       (OLD.origin='external_api' AND
        (ROW(NEW.tenant,NEW.repository,NEW.run,NEW.workspace,NEW.workspace_generation,NEW.epoch,
             NEW.principal,NEW.agent,NEW.task)
         IS DISTINCT FROM
         ROW(OLD.tenant,OLD.repository,OLD.run,OLD.workspace,OLD.workspace_generation,OLD.epoch,
             OLD.principal,OLD.agent,OLD.task)
         OR (OLD.state='closed' AND NEW IS DISTINCT FROM OLD)))
    THEN
        RAISE EXCEPTION 'TreeHarbor run origin or API identity cannot be replaced' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER th_api_agent_identity BEFORE UPDATE ON th_agent_runs
    FOR EACH ROW EXECUTE FUNCTION th_api_agent_identity();
