ALTER TABLE th_native_ref_updates DROP CONSTRAINT th_native_ref_updates_kind_check;
ALTER TABLE th_native_ref_updates ADD CHECK(kind IN ('create','policy','land','archive'));
ALTER TABLE th_native_ref_updates ADD CONSTRAINT th_native_archive_ref_update_check
    CHECK(kind <> 'archive' OR (generation > 1 AND old_head IS NOT NULL AND candidate IS NULL));
