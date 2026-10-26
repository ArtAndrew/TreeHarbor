-- Old captures remain readable, but have no inferred submitting credentials.
ALTER TABLE th_github_queue_reports
 ADD COLUMN reporter text,
 ADD COLUMN token uuid,
 ADD CONSTRAINT th_github_report_actor_pair CHECK ((reporter IS NULL) = (token IS NULL)),
 ADD CONSTRAINT th_github_report_principal FOREIGN KEY(tenant,reporter) REFERENCES th_principals(tenant,principal),
 ADD CONSTRAINT th_github_report_token FOREIGN KEY(tenant,token) REFERENCES th_access_tokens(tenant,token);
