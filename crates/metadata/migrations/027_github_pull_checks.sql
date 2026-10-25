-- A PR must pass required checks before GitHub accepts it into a merge queue.
-- Keep the existing queue source and its immutable v1 evidence unchanged.
ALTER TABLE th_github_managed_checks
    ALTER COLUMN queue DROP NOT NULL,
    ALTER COLUMN membership_sequence DROP NOT NULL,
    ADD COLUMN pull uuid,
    ADD CONSTRAINT th_github_managed_check_pull FOREIGN KEY(tenant,repository,pull)
        REFERENCES th_github_pulls(tenant,repository,pull),
    ADD CONSTRAINT th_github_managed_check_source CHECK(
        (queue IS NOT NULL AND membership_sequence IS NOT NULL AND pull IS NULL) OR
        (queue IS NULL AND membership_sequence IS NULL AND pull IS NOT NULL)
    );
