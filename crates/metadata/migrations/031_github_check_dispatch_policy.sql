-- Existing dispatches have no inferred certificate. No permissions are added.
CREATE INDEX th_github_managed_check_collections
 ON th_operations(tenant,repository,(result->>'check_run'),committed_at,key)
 WHERE kind='github.managed_check.collect.v1';
CREATE TABLE th_github_check_dispatch_policy (
 tenant text NOT NULL, repository uuid NOT NULL, publication uuid NOT NULL,
 queue uuid NOT NULL, host_policy_sequence bigint NOT NULL CHECK(host_policy_sequence BETWEEN 1 AND 4096),
 collection_key text NOT NULL,
 proof jsonb NOT NULL CHECK(proof->>'version'='1'),
 digest bytea NOT NULL CHECK(octet_length(digest)=32),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(tenant,repository,publication),
 FOREIGN KEY(tenant,repository,publication) REFERENCES th_github_check_publications(tenant,repository,publication),
 FOREIGN KEY(tenant,repository,queue,host_policy_sequence) REFERENCES th_github_queue_reports(tenant,repository,queue,sequence),
 FOREIGN KEY(tenant,repository,collection_key) REFERENCES th_operations(tenant,repository,key)
);
CREATE TRIGGER th_github_check_dispatch_policy_immutable BEFORE UPDATE OR DELETE ON th_github_check_dispatch_policy
 FOR EACH ROW EXECUTE FUNCTION th_reject_immutable_update();
