use super::*;
use treeharbor_merge::{PreparedRebase, RebaseOutcome, prepare_rebase};
use treeharbor_metadata::rebases::{RebaseLeases, RebaseStart};

async fn source(f: &mut Fixture, a: &Actors) -> (Uuid, Uuid, Authorization) {
    let entry = proposal(f, a, "source", b"agent edit\n").await;
    let change: Uuid = connection(&f.schema)
        .await
        .query_one(
            "SELECT changeset FROM th_native_queue WHERE entry=$1",
            &[&entry],
        )
        .await
        .unwrap()
        .get(0);
    (change, entry, auth(f, &a.owner, P::ChangeWrite).await)
}
async fn start_rebase(
    f: &mut Fixture,
    owner: &Authorization,
    change: Uuid,
    onto: ObjectId,
) -> Uuid {
    let id = Uuid::new_v4();
    let spec = RebaseStart {
        rebase: id,
        changeset: change,
        source_version: 1,
        onto,
    };
    let command = cmd(&format!("start-rebase-{id}"));
    let receipt =
        f.db.start_change_rebase(owner, &command, &spec)
            .await
            .unwrap();
    assert_eq!(
        receipt,
        f.db.start_change_rebase(owner, &command, &spec)
            .await
            .unwrap()
    );
    id
}
async fn compute(
    f: &mut Fixture,
    owner: &Authorization,
    id: Uuid,
) -> (PreparedRebase, BTreeMap<ObjectId, ObjectLease>) {
    let read = f.db.open_change_rebase(owner, 1, id).await.unwrap();
    for (index, node) in read.roots().into_iter().enumerate() {
        assert_eq!(
            f.db.read_change_rebase(&read, index, &f.depot, &[node], 1_000_000)
                .await
                .unwrap()
                .read()
                .unwrap()
                .len(),
            1
        );
    }
    let RebaseOutcome::Ready(result) =
        prepare_rebase(&f.stage, read.input(), Limits::default()).unwrap()
    else {
        panic!()
    };
    let mut leases = BTreeMap::new();
    let mut pending = vec![(result.revision(), false)];
    while let Some((id, done)) = pending.pop() {
        if leases.contains_key(&id) {
            continue;
        }
        let object = f.stage.get(id).unwrap();
        if !done {
            pending.push((id, true));
            pending.extend(object.references().into_iter().map(|(id, _)| (id, false)));
            continue;
        }
        let object = PreparedObject::new(&object).unwrap();
        let first =
            f.db.reserve_change_rebase_object(owner, 1, read_id(&read), &f.depot, &object)
                .await
                .unwrap();
        let second =
            f.db.reserve_change_rebase_object(owner, 1, read_id(&read), &f.depot, &object)
                .await
                .unwrap();
        assert_eq!(first.token(), second.token());
        assert_eq!(first.key().generation(), second.key().generation());
        if first.requires_upload() {
            let durable = f.depot.put(first.key().generation(), &object).unwrap();
            f.db.publish_object(&first, &durable).await.unwrap();
        }
        leases.insert(id, first);
    }
    f.db.close_change_rebase_read(read).await.unwrap();
    (*result, leases)
}
// The service-generated label is part of the immutable computation input.
fn read_id(read: &treeharbor_metadata::rebases::RebaseRead) -> Uuid {
    read.input()
        .label
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}
fn roots<'a>(r: &PreparedRebase, l: &'a BTreeMap<ObjectId, ObjectLease>) -> RebaseLeases<'a> {
    RebaseLeases {
        operations: &l[&r.applied().operations()],
        result: &l[&r.applied().result_snapshot()],
        revision: &l[&r.revision()],
    }
}
fn base_id(f: &Fixture) -> ObjectId {
    let RevisionRef::Native(id) = f.revision else {
        panic!()
    };
    id
}
async fn onto(f: &mut Fixture, path: &[u8], content: &[u8]) -> ObjectId {
    let blob = f.stage.put(&Object::Blob(content.to_vec())).unwrap();
    let ops = Changes::new(vec![ChangeOperation {
        path: RepoPath::new(path.to_vec()).unwrap(),
        expected: None,
        replacement: Some(EntryRef {
            mode: EntryMode::File,
            target: blob,
        }),
    }])
    .unwrap();
    let applied = prepare_change(&f.stage, f.base, &ops).unwrap();
    let commit = Commit {
        git_origin: None,
        parents: vec![base_id(f)],
        snapshot: applied.result_snapshot(),
        changes: applied.operations(),
        author_name: "Other agent".into(),
        author_email: "other@example.invalid".into(),
        timestamp_seconds: 42,
        timezone_minutes: 0,
        message: "new base".into(),
    };
    let id = f.stage.put(&Object::NativeCommit(commit.clone())).unwrap();
    let lease = upload_graph(&mut f.db, &f.depot, &f.stage, id).await;
    f.db.register_native_base(&context(&f.scope, &format!("onto-{id}")), &commit, &lease)
        .await
        .unwrap();
    id
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; immutable rebase lineage and admission"]
async fn rebase_preserves_run_and_versions_invalidates_old_evidence_and_survives_gc() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let (change, entry, owner) = source(&mut f, &a).await;
    let old = f.db.change_version(&f.scope, change, 1).await.unwrap();
    let (candidate, worker) = start(&mut f, &a, entry).await;
    prepare(&mut f, &worker, candidate).await;
    evidence(&mut f, &a, &worker, candidate).await;
    let onto = onto(&mut f, b"elsewhere", b"new base\n").await;
    let id = start_rebase(&mut f, &owner, change, onto).await;
    // Accepted immutable source remains usable when its original workspace drains.
    f.db.drain_workspace(&context(&f.scope, "drain-rebase"), f.workspace, 1)
        .await
        .unwrap();
    let (computed, leases) = compute(&mut f, &owner, id).await;
    assert_eq!(computed.commit().parents, vec![onto]);
    assert_ne!(computed.applied().base_snapshot(), f.base);
    let key = cmd("finish-rebase");
    let result =
        f.db.finish_change_rebase(&owner, &key, id, &computed, &roots(&computed, &leases))
            .await
            .unwrap();
    assert_eq!(result["result_version"], 2);
    assert_eq!(result["run"], f.run.to_string());
    assert_eq!(result["state"], "ready");
    assert_eq!(old, f.db.change_version(&f.scope, change, 1).await.unwrap());
    let new = f.db.change_version(&f.scope, change, 2).await.unwrap();
    assert_eq!(new["base_revision"], RevisionRef::Native(onto).to_string());
    assert_eq!(new["run"], old["run"]);
    let reader = auth(&mut f, &a.worker, P::Read).await;
    assert_eq!(
        f.db.native_candidate_status(&reader, candidate)
            .await
            .unwrap()["state"],
        "stale"
    );
    assert!(
        f.db.land_native_candidate(&worker, &cmd("land-old"), candidate)
            .await
            .is_err()
    );
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_change_rebase_lineage", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert!(
        raw.execute(
            "UPDATE th_change_rebase_lineage SET source_version=source_version",
            &[]
        )
        .await
        .is_err()
    );
    let next = Uuid::new_v4();
    let queue = auth(&mut f, &a.owner, P::QueueRequest).await;
    f.db.enqueue_native_change(&queue, &cmd("new-queue"), next, "main", change, 2)
        .await
        .unwrap();
    let (fresh, worker) = start(&mut f, &a, next).await;
    prepare(&mut f, &worker, fresh).await;
    assert!(matches!(
        f.db.land_native_candidate(&worker, &cmd("old-evidence"), fresh)
            .await,
        Err(MetaError::AdmissionBlocked)
    ));
    evidence(&mut f, &a, &worker, fresh).await;
    f.db.land_native_candidate(&worker, &cmd("land-new"), fresh)
        .await
        .unwrap();
    // Deliberately expire transient holds. Both immutable proposal versions and
    // the registered rebase graph remain protected by database pins.
    raw.execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        f.db.change_rebase_replay(&owner, &key, id).await.unwrap(),
        Some(result.clone())
    );
    for lease in leases.values() {
        assert!(
            f.db.try_retire_object(lease.key(), 0)
                .await
                .unwrap()
                .is_none()
        );
    }
    raw.execute("UPDATE th_repositories SET epoch=2", &[])
        .await
        .unwrap();
    assert_eq!(
        f.db.change_rebase_replay(&owner, &key, id).await.unwrap(),
        Some(result)
    );
    f.db.revoke_repository_token(&f.scope, a.owner.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.change_rebase_replay(&owner, &key, id).await,
        Err(MetaError::Unauthenticated)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; rebase race and authorization fences"]
async fn rebase_latest_cas_and_cancel_deny_late_writers_without_overwriting_source() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let (change, _, owner) = source(&mut f, &a).await;
    let base = base_id(&f);
    let first = start_rebase(&mut f, &owner, change, base).await;
    let second = start_rebase(&mut f, &owner, change, base).await;
    let (r1, l1) = compute(&mut f, &owner, first).await;
    let (r2, l2) = compute(&mut f, &owner, second).await;
    f.db.finish_change_rebase(&owner, &cmd("first"), first, &r1, &roots(&r1, &l1))
        .await
        .unwrap();
    assert!(matches!(
        f.db.finish_change_rebase(&owner, &cmd("second"), second, &r2, &roots(&r2, &l2))
            .await,
        Err(MetaError::VersionConflict)
    ));
    assert!(matches!(
        f.db.change_rebase_replay(&owner, &cmd("first"), second)
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    let cancelled =
        f.db.cancel_change_rebase(&owner, &cmd("cancel"), second)
            .await
            .unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(
        cancelled,
        f.db.cancel_change_rebase(&owner, &cmd("cancel"), second)
            .await
            .unwrap()
    );
    assert!(matches!(
        f.db.open_change_rebase(&owner, 1, second).await,
        Err(MetaError::RebaseState)
    ));
    assert_eq!(
        f.db.change_version(&f.scope, change, 2).await.unwrap()["run"],
        f.run.to_string()
    );
    assert!(matches!(
        f.db.change_version(&f.scope, change, 3).await,
        Err(MetaError::NotFound)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; rebase ownership, revoke, epoch and unsupported source fences"]
async fn rebase_permission_owner_epoch_and_expiry_are_checked_on_reads_and_acceptance() {
    for fence in ["owner", "revoke", "epoch", "cancel", "expired"] {
        let mut f = Fixture::new().await;
        let a = controls(&mut f).await;
        let (change, _, owner) = source(&mut f, &a).await;
        let base = base_id(&f);
        let id = start_rebase(&mut f, &owner, change, base).await;
        let (computed, leases) = compute(&mut f, &owner, id).await;
        let read = f.db.open_change_rebase(&owner, 1, id).await.unwrap();
        let raw = connection(&f.schema).await;
        let mut finish_auth = owner.clone();
        match fence {
            "owner" => {
                let other = actor(&mut f, "other", PrincipalKind::Agent, &[P::ChangeWrite]).await;
                let other = auth(&mut f, &other, P::ChangeWrite).await;
                assert!(matches!(
                    f.db.change_rebase_status(&other, 1, id).await,
                    Err(MetaError::Forbidden)
                ));
                assert!(matches!(
                    f.db.start_change_rebase(
                        &other,
                        &cmd("other-start"),
                        &RebaseStart {
                            rebase: Uuid::new_v4(),
                            changeset: change,
                            source_version: 1,
                            onto: base
                        }
                    )
                    .await,
                    Err(MetaError::Forbidden)
                ));
                finish_auth = other;
            }
            "revoke" => {
                f.db.revoke_repository_token(&f.scope, a.owner.id, "operator")
                    .await
                    .unwrap()
            }
            "epoch" => {
                raw.execute("UPDATE th_repositories SET epoch=2", &[])
                    .await
                    .unwrap();
            }
            "cancel" => {
                f.db.cancel_change_rebase(&owner, &cmd("cancel"), id)
                    .await
                    .unwrap();
            }
            "expired" => {
                // Privileged clock fixture only. Production cannot extend a deadline.
                raw.batch_execute("ALTER TABLE th_change_rebases DISABLE TRIGGER th_rebase_input_immutable; UPDATE th_change_rebases SET expires_at=clock_timestamp()-interval '1 second'; ALTER TABLE th_change_rebases ENABLE TRIGGER th_rebase_input_immutable").await.unwrap();
            }
            _ => unreachable!(),
        }
        if fence != "owner" {
            assert!(
                f.db.read_change_rebase(&read, 0, &f.depot, &[read.roots()[0].clone()], 1_000_000)
                    .await
                    .is_err(),
                "{fence}"
            );
            assert!(
                f.db.reserve_change_rebase_object(
                    &owner,
                    1,
                    id,
                    &f.depot,
                    &PreparedObject::new(&Object::Blob(b"late".to_vec())).unwrap()
                )
                .await
                .is_err(),
                "{fence}"
            );
        }
        assert!(
            f.db.finish_change_rebase(
                &finish_auth,
                &cmd("finish"),
                id,
                &computed,
                &roots(&computed, &leases)
            )
            .await
            .is_err(),
            "{fence}"
        );
        f.db.close_change_rebase_read(read).await.unwrap();
        assert!(matches!(
            f.db.change_version(&f.scope, change, 2).await,
            Err(MetaError::NotFound)
        ));
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; terminal conflicts and receipt replay"]
async fn rebase_conflict_is_durable_without_new_version_or_queue_invalidation() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let (change, entry, owner) = source(&mut f, &a).await;
    let target = onto(&mut f, b"src/file", b"conflicting edit\n").await;
    let id = start_rebase(&mut f, &owner, change, target).await;
    let read = f.db.open_change_rebase(&owner, 1, id).await.unwrap();
    assert!(matches!(
        prepare_rebase(&f.stage, read.input(), Limits::default()).unwrap(),
        RebaseOutcome::Conflicted(_)
    ));
    f.db.close_change_rebase_read(read).await.unwrap();
    let key = cmd("conflict");
    let result = f
        .db
        .finish_change_rebase_failure(&owner, &key, id, "merge_conflict", true, json!({"count":1}))
        .await
        .unwrap();
    assert_eq!(result["state"], "conflicted");
    assert_eq!(
        f.db.change_rebase_replay(&owner, &key, id).await.unwrap(),
        Some(result.clone())
    );
    assert_eq!(
        f.db.finish_change_rebase_failure(
            &owner,
            &key,
            id,
            "merge_conflict",
            true,
            json!({"count":1})
        )
        .await
        .unwrap(),
        result
    );
    let reader = auth(&mut f, &a.owner, P::Read).await;
    assert_eq!(
        f.db.native_queue_status(&reader, entry).await.unwrap()["state"],
        "queued"
    );
    assert!(matches!(
        f.db.change_version(&f.scope, change, 2).await,
        Err(MetaError::NotFound)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; populated schema-11 upgrade"]
async fn rebase_migration_preserves_accepted_versions_and_pending_checks() {
    use sha2::{Digest, Sha256};
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    connection("public")
        .await
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let raw = connection(&schema).await;
    let mut digest = Vec::new();
    for sql in [
        include_str!("../../../migrations/001_transactions.sql"),
        include_str!("../../../migrations/002_object_catalog.sql"),
        include_str!("../../../migrations/003_change_intake.sql"),
        include_str!("../../../migrations/004_access_control.sql"),
        include_str!("../../../migrations/005_source_profiles.sql"),
        include_str!("../../../migrations/006_source_uploads.sql"),
        include_str!("../../../migrations/007_workspace_preparations.sql"),
        include_str!("../../../migrations/008_managed_runs.sql"),
        include_str!("../../../migrations/009_change_uploads.sql"),
        include_str!("../../../migrations/010_native_integration.sql"),
        include_str!("../../../migrations/011_check_execution.sql"),
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut h = Sha256::new();
        h.update(digest);
        h.update(sql.as_bytes());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(11,$1)",
        &[&digest],
    )
    .await
    .unwrap();
    let scope = Scope::new("upgrade", Uuid::new_v4()).unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    let mut f = Fixture::from_db(schema, scope, db).await;
    let a = controls(&mut f).await;
    let entry = proposal(&mut f, &a, "before-upgrade", b"already accepted\n").await;
    let (cid, worker) = start(&mut f, &a, entry).await;
    prepare(&mut f, &worker, cid).await;
    evidence(&mut f, &a, &worker, cid).await;
    let query = "SELECT jsonb_build_object('versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY changeset,version) FROM th_change_versions v),'candidates',(SELECT jsonb_agg(to_jsonb(c) ORDER BY candidate) FROM th_native_candidates c),'refs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY name) FROM th_native_refs r),'checks',(SELECT jsonb_agg(to_jsonb(c) ORDER BY run) FROM th_native_check_runs c),'pins',(SELECT jsonb_agg(to_jsonb(p) ORDER BY holder) FROM th_object_pins p),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o))";
    let before: Value = raw.query_one(query, &[]).await.unwrap().get(0);
    assert!(matches!(
        f.db.check_schema().await,
        Err(MetaError::SchemaMismatch)
    ));
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    f.db.check_schema().await.unwrap();
    assert_eq!(
        before,
        raw.query_one(query, &[]).await.unwrap().get::<_, Value>(0)
    );
    evidence(&mut f, &a, &worker, cid).await;
    assert_eq!(
        f.db.land_native_candidate(&worker, &cmd("after-upgrade"), cid)
            .await
            .unwrap()["state"],
        "landed"
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; simultaneous original submission and rebase CAS"]
async fn ordinary_version_submission_racing_rebase_accepts_exactly_one_next_version() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let (change, _, owner) = source(&mut f, &a).await;
    let base = base_id(&f);
    let id = start_rebase(&mut f, &owner, change, base).await;
    let (r, leases) = compute(&mut f, &owner, id).await;
    let new = f.prepare(b"new source edit\n").await;
    let mut submission = submission(change, f.run, &new);
    submission.previous_version = 1;
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let ctx = context(&f.scope, "racing-submission");
    let key = cmd("racing-rebase");
    let roots = roots(&r, &leases);
    let (submitted, rebased) = tokio::join!(
        other.accept_change(&ctx, &submission),
        f.db.finish_change_rebase(&owner, &key, id, &r, &roots)
    );
    assert_ne!(submitted.is_ok(), rebased.is_ok());
    assert!(matches!(
        (submitted, rebased),
        (Ok(_), Err(MetaError::VersionConflict)) | (Err(MetaError::VersionConflict), Ok(_))
    ));
    assert_eq!(
        connection(&f.schema)
            .await
            .query_one(
                "SELECT count(*) FROM th_change_versions WHERE changeset=$1",
                &[&change]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    cleanup(&f.schema).await;
}
