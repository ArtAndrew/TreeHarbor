-- Owner policy is separate from provider evidence. No existing queue acquires approval.
CREATE TABLE th_github_check_policies (
 tenant text NOT NULL, repository uuid NOT NULL, binding uuid NOT NULL,
 epoch bigint NOT NULL CHECK(epoch>0), digest bytea NOT NULL CHECK(octet_length(digest)=32),
 definition bytea NOT NULL CHECK(octet_length(definition) BETWEEN 1 AND 32768),
 principal text NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant,repository,binding,epoch,digest),
 FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding)
);
CREATE TRIGGER th_github_check_policy_immutable BEFORE UPDATE OR DELETE ON th_github_check_policies FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

CREATE TABLE th_github_policy_refs (
 tenant text NOT NULL, repository uuid NOT NULL, binding uuid NOT NULL,
 epoch bigint NOT NULL CHECK(epoch>0), base_ref text NOT NULL CHECK(octet_length(base_ref) BETWEEN 1 AND 1024),
 generation bigint NOT NULL CHECK(generation>0), policy bytea,
 PRIMARY KEY(tenant,repository,binding,epoch,base_ref),
 FOREIGN KEY(tenant,repository,binding) REFERENCES th_github_bindings(tenant,repository,binding),
 FOREIGN KEY(tenant,repository,binding,epoch,policy) REFERENCES th_github_check_policies(tenant,repository,binding,epoch,digest)
);
CREATE FUNCTION th_github_policy_ref_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF ROW(NEW.tenant,NEW.repository,NEW.binding,NEW.epoch,NEW.base_ref)
 IS DISTINCT FROM ROW(OLD.tenant,OLD.repository,OLD.binding,OLD.epoch,OLD.base_ref)
 OR NEW.generation<>OLD.generation+1
 THEN RAISE EXCEPTION 'policy reference identity or generation changed' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER th_github_policy_ref_update BEFORE UPDATE ON th_github_policy_refs FOR EACH ROW EXECUTE FUNCTION th_github_policy_ref_update();
CREATE TRIGGER th_github_policy_ref_delete BEFORE DELETE ON th_github_policy_refs FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();

CREATE TABLE th_github_queue_policies (
 tenant text NOT NULL, repository uuid NOT NULL, queue uuid NOT NULL,
 binding uuid NOT NULL, epoch bigint NOT NULL CHECK(epoch>0), base_ref text NOT NULL,
 generation bigint NOT NULL CHECK(generation>0), policy bytea NOT NULL,
 PRIMARY KEY(tenant,repository,queue),
 FOREIGN KEY(tenant,repository,queue) REFERENCES th_github_queue(tenant,repository,queue),
 FOREIGN KEY(tenant,repository,binding,epoch,policy) REFERENCES th_github_check_policies(tenant,repository,binding,epoch,digest)
);
CREATE TRIGGER th_github_queue_policy_immutable BEFORE UPDATE OR DELETE ON th_github_queue_policies FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
